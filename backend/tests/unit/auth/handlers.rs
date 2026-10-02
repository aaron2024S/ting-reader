use super::*;

#[test]
fn default_admin_reminder_requires_both_default_credentials_and_admin_role() {
    let mut user = User {
        id: "test-admin".into(),
        username: DEFAULT_ADMIN_USERNAME.into(),
        password_hash: bcrypt::hash(DEFAULT_ADMIN_PASSWORD, 4).unwrap(),
        role: "admin".into(),
        created_at: String::new(),
    };
    assert!(
        user_info_from_user(&user)
            .unwrap()
            .uses_default_admin_credentials
    );

    user.password_hash = bcrypt::hash("changed-password", 4).unwrap();
    assert!(
        !user_info_from_user(&user)
            .unwrap()
            .uses_default_admin_credentials
    );

    user.password_hash = bcrypt::hash(DEFAULT_ADMIN_PASSWORD, 4).unwrap();
    user.username = "renamed-admin".into();
    assert!(
        !user_info_from_user(&user)
            .unwrap()
            .uses_default_admin_credentials
    );

    user.username = DEFAULT_ADMIN_USERNAME.into();
    user.role = "user".into();
    assert!(
        !user_info_from_user(&user)
            .unwrap()
            .uses_default_admin_credentials
    );
}
