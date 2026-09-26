#![cfg(feature = "daemon")]
#![allow(
    clippy::expect_used,
    reason = "a test that fails a precondition should fail loudly"
)]

use crownconnect_linux::pairing::ffsp_bridge::{
    derive_ffsp_link_secret, ffsp_device_id, FfspLink, FfspLinkStore,
};
use ffsp_linux::FileLinkStore;
use ffsp_protocol::device::{DeviceClass, DeviceId};
use ffsp_protocol::link::LinkStore;
use llts_signaling::device::DeviceClass as SignalingDeviceClass;

#[test]
fn ffsp_reads_the_links_the_bridge_writes() {
    let directory = std::env::temp_dir().join(format!(
        "crownconnect-ffsp-roundtrip-{}",
        std::process::id()
    ));
    let path = directory.join("links");
    let bridge = FfspLinkStore::new(&path);
    let phone_key = [0x51; 32];
    let tablet_key = [0x52; 32];
    let phone_secret = derive_ffsp_link_secret(&[0xaa; 32]);
    bridge
        .record(&FfspLink {
            public_key: phone_key,
            name: "Pixel 9",
            class: SignalingDeviceClass::Phone,
            secret: phone_secret.clone(),
        })
        .expect("record phone");
    bridge
        .record(&FfspLink {
            public_key: tablet_key,
            name: "Tab S9",
            class: SignalingDeviceClass::Tablet,
            secret: derive_ffsp_link_secret(&[0xbb; 32]),
        })
        .expect("record tablet");

    let store = FileLinkStore::open(&path).expect("ffsp opens the store");
    let phone = store
        .load(&DeviceId::from_public_key(&phone_key))
        .expect("load")
        .expect("ffsp knows the phone");
    assert_eq!(phone.name, "Pixel 9");
    assert_eq!(phone.class, DeviceClass::Phone);
    assert_eq!(phone.public_key, phone_key);
    assert_eq!(phone.secret.as_bytes(), phone_secret.as_bytes());
    assert_eq!(phone.device.as_bytes(), &ffsp_device_id(&phone_key));
    assert_eq!(store.all().expect("all").len(), 2);

    assert!(bridge.forget(&tablet_key).expect("forget"));
    let reopened = FileLinkStore::open(&path).expect("reopen");
    assert_eq!(reopened.all().expect("all").len(), 1);
    let _ = std::fs::remove_dir_all(directory);
}
