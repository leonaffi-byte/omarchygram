//! The single source of truth for Nerd Font UI glyphs.

pub const MENU: &str = "\u{f0c9}";
pub const SEARCH: &str = "\u{f002}";
pub const MORE: &str = "\u{f142}";
pub const INFO: &str = "\u{f129}";
pub const ATTACH: &str = "\u{f0c6}";
pub const EMOJI: &str = "\u{f118}";
pub const MIC: &str = "\u{f130}";
pub const MIC_OFF: &str = "\u{f131}";
pub const SEND: &str = "\u{f1d8}";
pub const CHECK: &str = "\u{f00c}";
pub const CHECK_DOUBLE: &str = "\u{f0139}";
pub const CLOCK: &str = "\u{f017}";
pub const PIN: &str = "\u{f08d}";
pub const MUTE: &str = "\u{f1f6}";
pub const ARCHIVE: &str = "\u{f187}";
pub const FORWARD: &str = "\u{f064}";
pub const REPLY: &str = "\u{f112}";
pub const TRASH: &str = "\u{f1f8}";
pub const CLOSE: &str = "\u{f00d}";
pub const DOWN: &str = "\u{f063}";
pub const LEFT: &str = "\u{f053}";
pub const RIGHT: &str = "\u{f054}";
pub const IMAGE: &str = "\u{f03e}";
pub const FILE: &str = "\u{f15b}";
pub const LINK: &str = "\u{f0c1}";
pub const USERS: &str = "\u{f0c0}";
pub const USER: &str = "\u{f007}";
pub const EYE: &str = "\u{f06e}";
pub const EDIT: &str = "\u{f044}";
pub const STICKER: &str = "\u{f1b2}";
pub const GIF: &str = "GIF";
pub const COPY: &str = "\u{f0c5}";
pub const SAVE: &str = "\u{f0c7}";
pub const STOP: &str = "\u{f04d}";
pub const GHOST: &str = "\u{f02a0}";
pub const COMPOSER_CURSOR: &str = "\u{258c}";
// ----- wave 6 (specs/spec-wave6.md §1.9) -----
pub const PLAY: &str = "\u{f04b}";
pub const PAUSE: &str = "\u{f04c}";
pub const VOLUME: &str = "\u{f028}";
pub const VOLUME_OFF: &str = "\u{f026}";
pub const FULLSCREEN: &str = "\u{f065}";
pub const LOCATION: &str = "\u{f041}";
pub const PHONE: &str = "\u{f095}";
pub const POLL: &str = "\u{f080}";
pub const DICE: &str = "\u{f522}";
pub const CALENDAR: &str = "\u{f073}";
pub const SCHEDULE: &str = "\u{f017}";
pub const TOPIC: &str = "\u{f292}";
pub const ROBOT: &str = "\u{f06a9}";
pub const CAMERA: &str = "\u{f030}";
pub const STORY: &str = "\u{f111}";
pub const LIVE: &str = "\u{f1eb}";
pub const ADD: &str = "\u{f067}";
pub const EXTERNAL: &str = "\u{f08e}";
pub const SPEED: &str = "\u{f0e7}";

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;

    const ICONS: &[&str] = &[
        MENU,
        SEARCH,
        MORE,
        INFO,
        ATTACH,
        EMOJI,
        MIC,
        MIC_OFF,
        SEND,
        CHECK,
        CHECK_DOUBLE,
        CLOCK,
        PIN,
        MUTE,
        ARCHIVE,
        FORWARD,
        REPLY,
        TRASH,
        CLOSE,
        DOWN,
        LEFT,
        RIGHT,
        IMAGE,
        FILE,
        LINK,
        USERS,
        USER,
        EYE,
        EDIT,
        STICKER,
        GIF,
        COPY,
        SAVE,
        STOP,
        GHOST,
        COMPOSER_CURSOR,
        PLAY,
        PAUSE,
        VOLUME,
        VOLUME_OFF,
        FULLSCREEN,
        LOCATION,
        PHONE,
        POLL,
        DICE,
        CALENDAR,
        SCHEDULE,
        TOPIC,
        ROBOT,
        CAMERA,
        STORY,
        LIVE,
        ADD,
        EXTERNAL,
        SPEED,
    ];

    #[test]
    fn nerd_font_contains_every_icon_codepoint() {
        let font = Path::new("/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf");
        if !font.exists() {
            return;
        }
        let output = match Command::new("fc-query")
            .arg("--format=%{charset}")
            .arg(font)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("failed to run fc-query: {error}"),
        };
        assert!(output.status.success(), "fc-query failed for {font:?}");
        let charset = String::from_utf8(output.stdout).expect("fc-query charset is UTF-8");
        for codepoint in ICONS
            .iter()
            .flat_map(|icon| icon.chars())
            .map(u32::from)
            .filter(|codepoint| *codepoint > 0x2000)
        {
            assert!(
                charset_contains(&charset, codepoint),
                "U+{codepoint:04X} is missing from {font:?}"
            );
        }
    }

    fn charset_contains(charset: &str, codepoint: u32) -> bool {
        charset.split_whitespace().any(|range| {
            let mut bounds = range
                .splitn(2, '-')
                .filter_map(|value| u32::from_str_radix(value, 16).ok());
            let Some(start) = bounds.next() else {
                return false;
            };
            let end = bounds.next().unwrap_or(start);
            (start..=end).contains(&codepoint)
        })
    }

    #[test]
    fn fontconfig_charset_ranges_are_parsed() {
        assert!(charset_contains("20-7e f000-f200", 0xf139));
        assert!(charset_contains("258c", 0x258c));
        assert!(!charset_contains("20-7e f000-f100", 0xf139));
    }
}
