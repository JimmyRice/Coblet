use super::{check_skin_png, sample};
use crate::PlayerError;

#[test]
fn accepts_only_skin_sizes() {
    assert_eq!(check_skin_png(&sample(64, 64)).unwrap(), (64, 64));
    assert_eq!(check_skin_png(&sample(64, 32)).unwrap(), (64, 32));
    for (width, height) in [(128, 128), (64, 48), (32, 64)] {
        assert!(matches!(
            check_skin_png(&sample(width, height)),
            Err(PlayerError::InvalidSkin(_))
        ));
    }
    assert!(check_skin_png(b"GIF89a").is_err());
    assert!(check_skin_png(&sample(64, 64)[..18]).is_err());
    assert!(check_skin_png(&vec![0; 300 * 1024]).is_err());
}
