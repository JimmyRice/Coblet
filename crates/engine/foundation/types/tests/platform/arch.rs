use super::Arch;

#[test]
fn maps_aliases() {
    for (alias, arch) in [
        ("amd64", Arch::X86_64),
        ("AMD64", Arch::X86_64),
        ("i686", Arch::X86),
        ("arm64", Arch::Aarch64),
        ("powerpc64le", Arch::Ppc64le),
        ("riscv64", Arch::Riscv64),
        ("arm", Arch::Arm),
    ] {
        assert_eq!(alias.parse::<Arch>().unwrap(), arch, "{alias}");
    }
}

#[test]
fn keeps_unknown_architectures() {
    for name in ["sparc64", "loongarch64"] {
        let arch: Arch = name.parse().unwrap();
        assert_eq!(arch.as_str(), name);
        assert!(matches!(arch, Arch::Other(_)), "{name}");
    }
}
