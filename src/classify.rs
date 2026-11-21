//! Decide which virtual input an app's audio belongs to.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Game,
    Chat,
    Media,
}

impl Channel {
    pub fn sink(self) -> &'static str {
        match self {
            Channel::Game => "arctis_game",
            Channel::Chat => "arctis_chat",
            Channel::Media => "arctis_media",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Game => "game",
            Channel::Chat => "chat",
            Channel::Media => "media",
        }
    }
    pub fn parse(s: &str) -> Option<Channel> {
        match s {
            "game" => Some(Channel::Game),
            "chat" => Some(Channel::Chat),
            "media" => Some(Channel::Media),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppInfo {
    /// Stable per-app identity (used for overrides and caching).
    pub key: String,
    /// Launched by Steam/umu (`SteamGameId` / `GAMEID` in its environment).
    pub launcher_game: bool,
    /// Lower-case executable names / path suffixes to look up in the game DB.
    pub names: Vec<String>,
    /// freedesktop `.desktop` Categories of the app, if found.
    pub categories: Vec<String>,
}

/// Override → launcher env → game DB → .desktop category → Media.
pub fn classify(app: &AppInfo, override_: Option<Channel>, in_db: impl Fn(&[String]) -> bool) -> Channel {
    if let Some(c) = override_ {
        return c;
    }
    if app.launcher_game || in_db(&app.names) {
        return Channel::Game;
    }
    let has = |c: &str| app.categories.iter().any(|x| x == c);
    if has("InstantMessaging") || has("Chat") {
        Channel::Chat
    } else if has("Game") {
        Channel::Game
    } else {
        Channel::Media
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppInfo {
        AppInfo { key: "x".into(), launcher_game: false, names: vec!["x.exe".into()], categories: vec![] }
    }
    fn no_db(_: &[String]) -> bool {
        false
    }
    fn db(_: &[String]) -> bool {
        true
    }

    #[test]
    fn override_wins() {
        let a = AppInfo { launcher_game: true, ..app() };
        assert_eq!(classify(&a, Some(Channel::Chat), db), Channel::Chat);
    }

    #[test]
    fn launcher_env_is_game() {
        let a = AppInfo { launcher_game: true, ..app() };
        assert_eq!(classify(&a, None, no_db), Channel::Game);
    }

    #[test]
    fn db_hit_is_game() {
        assert_eq!(classify(&app(), None, db), Channel::Game);
    }

    #[test]
    fn desktop_chat() {
        let a = AppInfo { categories: vec!["Network".into(), "InstantMessaging".into()], ..app() };
        assert_eq!(classify(&a, None, no_db), Channel::Chat);
        let a = AppInfo { categories: vec!["Chat".into()], ..app() };
        assert_eq!(classify(&a, None, no_db), Channel::Chat);
    }

    #[test]
    fn desktop_game() {
        let a = AppInfo { categories: vec!["Game".into(), "ActionGame".into()], ..app() };
        assert_eq!(classify(&a, None, no_db), Channel::Game);
    }

    #[test]
    fn unknown_is_media() {
        let a = AppInfo { categories: vec!["Network".into(), "WebBrowser".into()], ..app() };
        assert_eq!(classify(&a, None, no_db), Channel::Media);
    }

    #[test]
    fn order_db_before_desktop() {
        let a = AppInfo { categories: vec!["InstantMessaging".into()], ..app() };
        assert_eq!(classify(&a, None, db), Channel::Game);
    }

    #[test]
    fn channel_roundtrip() {
        for c in [Channel::Game, Channel::Chat, Channel::Media] {
            assert_eq!(Channel::parse(c.as_str()), Some(c));
        }
        assert_eq!(Channel::Game.sink(), "arctis_game");
        assert_eq!(Channel::Chat.sink(), "arctis_chat");
        assert_eq!(Channel::Media.sink(), "arctis_media");
        assert_eq!(Channel::parse("nope"), None);
    }
}
