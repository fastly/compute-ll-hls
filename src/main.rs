//! POC for serving LL-HLS on Compute@Edge.
mod ll_hls_skip;
use crate::ll_hls_skip::collapse_skipped;

use fastly::http::{header, Method, StatusCode};
use fastly::{Error, Request, Response};
use std::collections::HashMap;

/// Names of backend servers associated with this service.
/// These are the names of the origin servers provided for the Fastly service
/// running this app. In test, these are overridden in `backends_dev.toml`.
const BACKEND_NAME: &str = "video_backend";
const BACKEND_ALT: &str = "video_backend_alt";
/// Prefix to redirect requests to `video_backend_alt` backend.
const ALT_PATH_PREFIX: &str = "/alt";
/// Name of the paths to the LL-HLS playlists. Used for displaying a simple homepage.
const BACKEND_PLAYLIST_PATH: &str = "/LowLatencyBBB/myStream/playlist.m3u8";
const BACKEND_ALT_PLAYLIST_PATH: &str = "/LowLatencyBBB-EU/myStream/playlist.m3u8";

/// home returns HTML for a simple homepage.
fn home() -> Result<(), Error> {
    let html = format!(
        "
        <!DOCTYPE html>
        <html>
        <body>

        <h1>LL-HLS on Compute @ Edge</h1>
        <p><b>Hint: Open below in Safari/Quicktime:</b></p>
        <a href=\"{}\">Wowza stream</a><br>
        <a href=\"{}{}\">Wowza EU stream</a><br>
        </body>
        </html>",
        BACKEND_PLAYLIST_PATH,
        ALT_PATH_PREFIX,
        BACKEND_ALT_PLAYLIST_PATH,
    );

    let home_resp = Response::from_body(html);
    home_resp.send_to_client();
    Ok(())
}

/// handle_req deals with making the request to the appropriate backend.
/// For delta playlist requests, performs
fn handle_req(mut req: Request) -> Result<(), Error> {
    let mut backend = BACKEND_NAME;

    let mut new = req.clone_with_body();
    // Switch the backend based on the path prefix.
    if req.get_path().starts_with(ALT_PATH_PREFIX) {
        backend = BACKEND_ALT;
        if let Some(rest) = req.get_path().get(ALT_PATH_PREFIX.len()..) {
            new.set_path(rest);
        }
    }

    // Can't unzip gzip yet in C@E.
    new.remove_header_str(header::ACCEPT_ENCODING);

    // Respond to skip param if specified.
    let qp: HashMap<String, String> = req.get_query().unwrap();
    let skip_val = match qp.get("_HLS_skip") {
        Some(sv) => sv,
        None => "",
    };

    // Don't cache playlists, although they should ideally be cached w/ 500ms TTL.
    // TODO(@phu): Revisit when we can specify 500ms TTL in C@E.
    if req.get_path().ends_with(".m3u8") {
        new.set_pass(true);
    }

    if skip_val == "YES" || skip_val == "v2" {
        let mut query_params = qp.clone();
        // Request the playlist without a skip param,
        // so origin doesn't calculate a delta playlist
        query_params.remove("_HLS_skip");
        new.set_query(&query_params)?;
        let mut be_resp = new.send(backend)?;
        let mut new_resp = be_resp.clone_with_body();
        let delta_playlist = collapse_skipped(skip_val, new_resp.take_body().into_string());
        new_resp.set_body(delta_playlist);
        new_resp.send_to_client();
    } else {
        let be_resp = new.send(backend)?;
        be_resp.send_to_client();
    }

    Ok(())
}

fn main() -> Result<(), Error> {
    let req = Request::from_client();

    // Filter request methods.
    match req.get_method() {
        // Allow GET and HEAD requests.
        &Method::GET | &Method::HEAD => {
            if req.get_path() == "/" {
                home()
            } else {
                handle_req(req)
            }
        }
        // Deny anything else.
        _ => {
            let response = Response::from_status(StatusCode::METHOD_NOT_ALLOWED)
                .with_header(header::ALLOW, "GET, HEAD")
                .with_body_str("This method is not allowed\n");
            response.send_to_client();
            Ok(())
        }
    }
}
