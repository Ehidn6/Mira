use std::{fs, process::Command};

#[test]
fn state_image_is_provisioned_once_and_unknown_files_are_preserved() {
    let dir = std::env::temp_dir().join(format!("mira-volume-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state, with spaces.img");
    let mut cmd = Command::new("qemu-system-x86_64");
    mira_lang::uefi::attach_state_disk(&mut cmd, &path).unwrap();
    let mut expected = fs::read(&path).unwrap();
    assert_eq!(expected.len(), 2 * 1024 * 1024);
    assert_eq!(&expected[..8], b"MIRAVOL2");
    expected[4096] = 77;
    fs::write(&path, &expected).unwrap();
    mira_lang::uefi::attach_state_disk(&mut Command::new("qemu-system-x86_64"), &path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), expected);
    let prefix = expected[..1024 * 1024].to_vec();
    fs::write(&path, &prefix).unwrap();
    mira_lang::uefi::attach_state_disk(&mut Command::new("qemu-system-x86_64"), &path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), expected);
    expected[0] = 0;
    fs::write(&path, &expected).unwrap();
    assert!(
        mira_lang::uefi::attach_state_disk(&mut Command::new("qemu-system-x86_64"), &path).is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), expected);
    fs::remove_dir_all(dir).unwrap();
}
