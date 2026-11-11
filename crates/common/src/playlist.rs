//! The playlist listing yt-dlp prints. Node's dashboard and the supervisor read the same text, so their indexes match.

/// `--flat-playlist --print` template: id, title, duration, tab-separated.
pub const PRINT_TEMPLATE: &str = "%(id)s\t%(title)s\t%(duration)s";

#[derive(Clone, Debug, PartialEq)]
pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub duration: Option<f64>,
}

/// `PRINT_TEMPLATE` lines. YouTube keeps private and deleted videos
/// in a list under these titles; they can never play, so they never enter it.
pub fn parse_playlist(out: &str) -> Vec<PlaylistEntry> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let id = f.next()?.trim().to_string();
            (!id.is_empty()).then(|| PlaylistEntry { id, title: f.next().unwrap_or("").to_string(), duration: f.next().and_then(|d| d.trim().parse().ok()) })
        })
        .filter(|e| e.title != "[Private video]" && e.title != "[Deleted video]")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_playlist_reads_id_title_and_duration() {
        // Act
        let e = parse_playlist("abc\tFirst\t61.5\ndef\tSecond\tNA\n\nghi\tThird\n");
        // Assert
        assert_eq!(e.len(), 3);
        assert_eq!((e[0].id.as_str(), e[0].title.as_str(), e[0].duration), ("abc", "First", Some(61.5)));
        assert_eq!(e[1].duration, None);
        assert_eq!((e[2].title.as_str(), e[2].duration), ("Third", None));
    }

    #[test]
    fn parse_playlist_drops_private_and_deleted_videos() {
        // Act
        let e = parse_playlist("a\t[Private video]\tNA\nb\t[Deleted video]\tNA\nc\tKept\t10\n");
        // Assert
        assert_eq!(e.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["c"]);
    }

    #[test]
    fn print_template_has_three_tab_separated_fields() {
        // Assert
        assert_eq!(PRINT_TEMPLATE.split('\t').count(), 3);
    }
}
