//! Functionality for parsing and generating LL-HLS Delta playlists.
use std::cmp::max;
use std::fs;
use std::fmt;
use std::path::Path;

/// SkipParams controls how a Delta playlist is generated.
#[derive(Debug, PartialEq)]
struct SkipParams {
    /// Controls what gets skipped: segments or both segments and dateranges.
    skip_param: String,
    /// Segments or dateranges with offset_seconds older than this
    /// should be skipped.
    offset_cutoff_seconds: f64,
    /// Whether or not dateranges can be skipped.
    /// This value is not the same as `skip_param`, as it is provided by the server.
    /// Both this and skip_param == v2 are needed to actually skip dateranges.
    can_skip_dateranges: bool,
}

impl SkipParams {
    /// Defines a noop set of params that don't skip anything
    /// when used with should_skip functions.
    fn noop() -> Self {
        SkipParams {
            skip_param: "".to_string(),
            offset_cutoff_seconds: 0f64,
            can_skip_dateranges: false,
        }
    }
}

/// SkippableSegment defines a segment or partial segment that could be skipped.
struct SkippableSegment {
    /// offset_seconds describes where this segment is in time,
    /// relative to the beginning of the playlist.
    /// It is the sum of its duration and the durations before it.
    offset_seconds: f64,
    /// playlist_idx locates this segment in the playlist.
    playlist_idx: usize,
}

impl SkippableSegment {
    fn should_skip(&self, skip_params: &SkipParams) -> bool {
        (skip_params.skip_param == "v2" || skip_params.skip_param == "YES")
            && skip_params.offset_cutoff_seconds > self.offset_seconds
    }
}

/// SkippableDaterange defines a daterange that could be skipped.
struct SkippableDaterange {
    /// offset_seconds describes where this daterange is in time,
    /// relative to the beginning of the playlist.
    /// It is the sum of its duration and the durations before it.
    offset_seconds: f64,
    /// Corresponds to the ID value on a daterange.
    id: String,
    ///  playlist_idx locates this daterange in the playlist.
    playlist_idx: usize,
}

impl SkippableDaterange {
    fn should_skip(&self, skip_params: &SkipParams) -> bool {
        skip_params.can_skip_dateranges
        && skip_params.skip_param == "v2"
        && skip_params.offset_cutoff_seconds > self.offset_seconds
    }
}

/// SkippedSegments contains all the information needed to write an #EXT-X-SKIP line.
struct SkippedSegments {
    num_skipped_segments: u32,
    skipped_daterange_ids: Vec<String>,
}

impl fmt::Display for SkippedSegments {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut str_parts: Vec<String> = Vec::new();

        if self.num_skipped_segments > 0 {
            str_parts.push(format!("SKIPPED-SEGMENTS={}", self.num_skipped_segments));
        }

        if self.skipped_daterange_ids.len() > 0 {
            str_parts.push(format!(
                "RECENTLY-REMOVED-DATERANGES={}",
                self.skipped_daterange_ids.join("\t")
            ));
        }

        if str_parts.len() > 0 {
            write!(f, "#EXT-X-SKIP:{}\n", str_parts.join(","))
        } else {
            write!(f, "")
        }
    }
}

/// parse_playlist parses the given LL-HLS playlist for tags controlling skip behavior.
/// Returns the parameters corresponding to how the delta playlist should be calculated,
/// as well as media segments and dateranges that are eligible to be skipped.
fn parse_playlist(
    skip_val: &str,
    playlist: String,
) -> (SkipParams, Vec<SkippableSegment>, Vec<SkippableDaterange>) {
    let mut segments: Vec<SkippableSegment> = Vec::new();
    let mut dateranges: Vec<SkippableDaterange> = Vec::new();
    let mut skip_boundary_seconds = 0f64;
    let mut skip_date_ranges = false;
    let mut total_duration = 0f64;

    // NOTE: Only full media segments are skippable, and thus returned by this function.
    // Playlists may contain partial segments but if their parent media segment is "complete"
    // (i.e. no more partial segments will be added to it), then that parent segment will follow.
    //
    // Example (MSN=437107):
    //  #EXT-X-PART:DURATION=1.00000,INDEPENDENT=YES,URI="lowLatencyHLS.php?segment=filePart437107.1.ts"
    //  #EXT-X-PART:DURATION=1.00000,INDEPENDENT=YES,URI="lowLatencyHLS.php?segment=filePart437107.2.ts"
    //  #EXT-X-PART:DURATION=1.00000,INDEPENDENT=YES,URI="lowLatencyHLS.php?segment=filePart437107.3.ts"
    //  #EXT-X-PART:DURATION=1.00000,INDEPENDENT=YES,URI="lowLatencyHLS.php?segment=filePart437107.4.ts"
    //  #EXTINF:4.00000,
    //  fileSequence437107.ts <--- this is the parent of the four above partial segments,
    //                             and will be the only segment returned
    //
    // Additionally, partial segments that go off the end (and aren't yet complete, i.e. aren't
    // followed yet by their corresponding parent segment) wouldn't be skipped,
    // as the Skip Boundary must be 6x the target duration. So we don't add those at all.
    // (Ref: https://tools.ietf.org/html/draft-pantos-hls-rfc8216bis-08#section-4.4.3.8)

    // Keep track of the duration of the last group of partial segments.
    // We need this to calculate the playlist's total duration.
    let mut last_partial_segment_duration = 0f64;

    for (i, line) in playlist.lines().enumerate() {
        if line.starts_with("#EXT-X-SERVER-CONTROL:") {
            // Filter for tags that control what can be skipped.
            let line_split: Vec<&str> = line.split(":").collect();
            let kvs = line_split[1].split(",");
            for kv in kvs {
                let split: Vec<&str> = kv.split("=").collect();
                match split[0] {
                    "CAN-SKIP-UNTIL" => {
                        skip_boundary_seconds = split[1].parse().unwrap();
                    }
                    "CAN-SKIP-DATERANGES" => {
                        skip_date_ranges = split[1].parse().unwrap();
                    }
                    _ => (),
                }
            }
        } else if line.starts_with("#EXTINF:") {
            // Indicates a media segment:
            //   #EXTINF:4.00008,
            //   fileSequence269.mp4

            // "Erase" the duration of the last partial segment,
            // since any media segment coming after partial segments means
            // those partial segments weren't the last group of partial segments.
            if last_partial_segment_duration > 0f64 {
                last_partial_segment_duration = 0f64;
            }

            let line_split: Vec<&str> = line.split(":").collect();
            let vals_split: Vec<&str> = line_split[1].split(",").collect();
            let duration: f64 = vals_split[0].parse().unwrap();
            total_duration += duration;

            let segment = SkippableSegment {
                offset_seconds: total_duration,
                playlist_idx: i,
            };

            segments.push(segment);
        } else if line.starts_with("#EXT-X-PART:") {
            // Indicates a partial media segment.
            //   #EXT-X-PART:DURATION=2.00004,INDEPENDENT=YES,URI="filePart271.0.mp4"
            //   or
            //   #EXT-X-PART:DURATION=2.00004,URI="filePart271.1.mp4"
            // We don't return these, but we need to record
            let line_split: Vec<&str> = line.split(":").collect();
            let line_vals_split: Vec<&str> = line_split[1].split(",").collect();
            for val in line_vals_split.iter() {
                let tag_val: Vec<&str> = val.split("=").collect();
                if tag_val[0] == "DURATION" {
                    let duration: f64 = tag_val[1].parse().unwrap();
                    last_partial_segment_duration += duration;
                    break;
                }
            }
        } else if line.starts_with("#EXT-X-DATERANGE") {
            //    #EXT-X-DATERANGE:ID="splice-6FFFFFF0",START-DATE="2014-03-05T11:
            //    15:00Z",PLANNED-DURATION=59.993,SCTE35-OUT=...
            let line_split: Vec<&str> = line.split(":").collect();
            let line_vals_split: Vec<&str> = line_split[1].split(",").collect();
            let mut duration = 0f64;
            let mut id = "";

            for val in line_vals_split.iter() {
                let tag_val: Vec<&str> = val.split("=").collect();

                match tag_val[0] {
                    "DURATION" => duration = tag_val[1].parse().unwrap(),
                    "ID" => id = tag_val[1],
                    _ => (),
                }
            }

            total_duration += duration;
            let daterange = SkippableDaterange {
                offset_seconds: total_duration,
                id: id.to_string(),
                playlist_idx: i,
            };
            dateranges.push(daterange);
        } else if line.starts_with("#EXT-X-ENDLIST") {
            // No skipping should happen in this case, return a no-op default.
            return (SkipParams::noop(), segments, dateranges);
        }
        // NOTE: There's technically an extra case here since #EXT-X-VERSION must be >= 9
        // for skipping to happen and >= 10 for dateranges to be skipped.
        // But, I've found playlists that respond to _HLS_skip with version < 9...
    }

    let skip_params = SkipParams {
        skip_param: skip_val.into(),
        offset_cutoff_seconds: last_partial_segment_duration + total_duration
            - skip_boundary_seconds,
        can_skip_dateranges: skip_date_ranges,
    };
    (skip_params, segments, dateranges)
}

/// collapse_skipped parses the given playlist and adds applies a delta transformation if possible:
/// - adds `#EXT-X-SKIP` tag to the playlist
/// - removes the segments and optionally dateranges that were skipped.
/// Returns a delta playlist if one was generated, otherwise it returns the original playlist.
pub(crate) fn collapse_skipped(skip_val: &str, playlist: String) -> String {
    let playlist_lines = playlist.clone();
    // Parse the playlist for skip parameters, segments and dateranges.
    let (skip_params, segments, dateranges) = parse_playlist(skip_val, playlist);

    // Figure out what #EXT-X-SKIP is supposed to look like.
    let mut first_non_skipped_idx = None;
    let mut num_skipped_segments: u32 = 0;
    for seg in &segments {
        if seg.should_skip(&skip_params) {
            num_skipped_segments += 1;
        } else if first_non_skipped_idx == None {
            first_non_skipped_idx = Some(seg.playlist_idx);
        }
    }

    let mut skipped_daterange_ids: Vec<String> = Vec::new();
    for dr in &dateranges {
        if dr.should_skip(&skip_params) {
            skipped_daterange_ids.push(dr.id.clone());
        } else if first_non_skipped_idx == None {
            first_non_skipped_idx = Some(dr.playlist_idx);
        }
    }

    let skipped_segments = SkippedSegments {
        num_skipped_segments: num_skipped_segments,
        skipped_daterange_ids: skipped_daterange_ids,
    };

    // Find where to insert #EXT-X-SKIP. This should be at the index
    // of the first non-skipped segment or daterange in the original playlist.
    let skip_line_idx;
    match first_non_skipped_idx {
        Some(idx) => skip_line_idx = idx,
        None => {
            // Everything or nothing was skipped
            if num_skipped_segments > 0 {
                let last_skipped_idx = max(
                    segments[segments.len() - 1].playlist_idx,
                    dateranges[dateranges.len() - 1].playlist_idx,
                );
                skip_line_idx = last_skipped_idx;
            } else {
                // Don't append skip line, just return playlist
                return playlist_lines;
            }
        }
    }

    let mut playlist_with_skipped = String::new();
    for (i, line) in playlist_lines.lines().enumerate() {
        if line.starts_with("#EXT")
            && !(line.starts_with("#EXTINF:")
                || line.starts_with("#EXT-X-PART:")
                || line.starts_with("#EXT-X-DATERANGE:")
                || line.starts_with("#EXT-X-PROGRAM-DATE-TIME:"))
        {
            // Write all non-skippable lines. Note that all of these should have some #EXT tag.
            if line.starts_with("#EXT-X-MEDIA-SEQUENCE") {
                // Write #EXT-X-SKIP right after.
                playlist_with_skipped.push_str(format!("{}\n", line).as_str());
                let st = skipped_segments.to_string();
                playlist_with_skipped.push_str(st.as_str());
            } else if line.starts_with("#EXT-X-VERSION") {
                playlist_with_skipped.push_str("#EXT-X-VERSION:9\n");
            } else {
                playlist_with_skipped.push_str(format!("{}\n", line).as_str());
            }
        } else if i >= skip_line_idx {
            playlist_with_skipped.push_str(format!("{}\n", line).as_str());
        }
    }

    playlist_with_skipped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn non_delta_playlist() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/regular.m3u8");
        fs::read_to_string(path).unwrap()
    }

    fn delta_playlist() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/delta.m3u8");
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn test_parse_playlist() {
        let in_playlist = non_delta_playlist();

        // No skippable dateranges
        let (params, segments, dateranges) = parse_playlist("YES", in_playlist);
        assert_eq!(dateranges.len(), 0);
        assert_eq!(segments.len(), 7);
        assert_eq!(
            params,
            SkipParams {
                skip_param: String::from("YES"),
                offset_cutoff_seconds: 17.33392,
                can_skip_dateranges: false
            }
        );
    }

    #[test]
    fn test_write_delta_playlist() {
        let in_playlist = non_delta_playlist();
        let out_playlist = delta_playlist();
        let generated_playlist = collapse_skipped("YES", in_playlist);

        assert_eq!(
            out_playlist, generated_playlist,
            "\n\
                   Expected playlist:\n\
                        {}\n\
                   Got playlist:\n\
                        {}\n",
            out_playlist, generated_playlist
        );
    }
}
