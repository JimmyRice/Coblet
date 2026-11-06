use super::GameLayout;

#[test]
fn makes_relative_paths_absolute() {
    let layout = GameLayout::new("data", "games/a");
    assert!(layout.data_root().is_absolute());
    assert!(layout.game_directory().ends_with("games/a"));
    assert_eq!(
        layout.library("a/b/c.jar"),
        layout
            .data_root()
            .join("libraries")
            .join("a")
            .join("b")
            .join("c.jar")
    );
}
