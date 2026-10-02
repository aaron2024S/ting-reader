use super::UserSettingsResponse;
use crate::db::models::UserSettings;

fn settings(settings_json: Option<&str>) -> UserSettings {
    UserSettings {
        user_id: "user-1".to_string(),
        playback_speed: 1.0,
        theme: "auto".to_string(),
        auto_play: 1,
        skip_intro: 0,
        skip_outro: 0,
        settings_json: settings_json.map(str::to_string),
        updated_at: "2026-07-28T00:00:00Z".to_string(),
    }
}

#[test]
fn defaults_bookshelf_cover_shape_to_square() {
    let response = UserSettingsResponse::from(settings(None));

    assert_eq!(
        response
            .settings_json
            .as_ref()
            .and_then(|value| value["bookshelf_cover_shape"].as_str()),
        Some("square")
    );
}

#[test]
fn preserves_explicit_bookshelf_cover_shape() {
    let response =
        UserSettingsResponse::from(settings(Some(r#"{"bookshelf_cover_shape":"rect"}"#)));

    assert_eq!(
        response
            .settings_json
            .as_ref()
            .and_then(|value| value["bookshelf_cover_shape"].as_str()),
        Some("rect")
    );
}
