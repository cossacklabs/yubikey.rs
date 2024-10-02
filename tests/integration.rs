//! Integration tests

#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms, trivial_casts, unused_qualifications)]

use log::trace;
use once_cell::sync::Lazy;
use rand_core::{OsRng, RngCore};
use rsa::{pkcs1v15, RsaPublicKey};
use sha2::{Digest, Sha256};
use signature::hazmat::PrehashVerifier;
use std::{env, str::FromStr, sync::Mutex, time::Duration};
use x509_cert::{der::Encode, name::Name, serial_number::SerialNumber, time::Validity};
use yubikey::{
    certificate::yubikey_signer,
    certificate::Certificate,
    piv::{self, AlgorithmId, Key, ManagementAlgorithmId, ManagementSlotId, RetiredSlotId, SlotId},
    Error, MgmKey3Des, MgmKeyAes192, PinPolicy, Serial, TouchPolicy, YubiKey,
};
#[cfg(feature = "untested")]
use yubikey::{MgmKey, MgmKeyAlgorithm};

static YUBIKEY: Lazy<Mutex<YubiKey>> = Lazy::new(|| {
    // Only show logs if `RUST_LOG` is set
    if env::var("RUST_LOG").is_ok() {
        env_logger::builder().format_timestamp(None).init();
    }

    let yubikey = if let Ok(serial) = env::var("YUBIKEY_SERIAL") {
        let serial = Serial::from_str(&serial).unwrap();
        YubiKey::open_by_serial(serial).unwrap()
    } else {
        YubiKey::open().unwrap()
    };

    trace!("serial: {}", yubikey.serial());
    trace!("version: {}", yubikey.version());

    Mutex::new(yubikey)
});

//
// CCCID support
//

#[test]
#[ignore]
fn test_get_cccid() {
    let mut yubikey = YUBIKEY.lock().unwrap();

    match yubikey.cccid() {
        Ok(cccid) => trace!("CCCID: {:?}", cccid),
        Err(Error::NotFound) => trace!("CCCID not found"),
        Err(err) => panic!("error getting CCCID: {:?}", err),
    }
}

//
// CHUID support
//

#[test]
#[ignore]
fn test_get_chuid() {
    let mut yubikey = YUBIKEY.lock().unwrap();

    match yubikey.chuid() {
        Ok(chuid) => trace!("CHUID: {:?}", chuid),
        Err(Error::NotFound) => trace!("CHUID not found"),
        Err(err) => panic!("error getting CHUID: {:?}", err),
    }
}

//
// Device config support
//

#[test]
#[ignore]
fn test_get_config() {
    let mut yubikey = YUBIKEY.lock().unwrap();
    let config_result = yubikey.config();
    assert!(config_result.is_ok());
    trace!("config: {:?}", config_result.unwrap());
}

//
// Cryptographic key support
//

#[test]
#[ignore]
fn test_list_keys() {
    let mut yubikey = YUBIKEY.lock().unwrap();
    let keys_result = Key::list(&mut yubikey);
    assert!(keys_result.is_ok());
    trace!("keys: {:?}", keys_result.unwrap());
}

//
// PIN support
//

#[test]
#[ignore]
fn test_verify_pin() {
    let mut yubikey = YUBIKEY.lock().unwrap();
    assert!(yubikey.verify_pin(b"000000").is_err());
    assert!(yubikey.verify_pin(b"123456").is_ok());
}

fn get_mgm_key_meta(yubikey: &mut YubiKey) -> piv::SlotMetadata {
    piv::metadata(yubikey, SlotId::Management(ManagementSlotId::Management)).unwrap()
}

/// Given a default YubiKey, authenticate with the default management key.
///
/// This is slightly complicated by newer firmwares using AES192 as the MGM default key,
/// over 3DES.
fn auth_default_mgm(yubikey: &mut YubiKey) {
    match get_mgm_key_meta(yubikey).algorithm {
        ManagementAlgorithmId::ThreeDes => {
            assert!(yubikey.authenticate(MgmKey3Des::default()).is_ok())
        }
        ManagementAlgorithmId::Aes192 => {
            assert!(yubikey.authenticate(MgmKeyAes192::default()).is_ok())
        }
        other => panic!("unexpected management key algorithm: {:?}", other),
    }
}

//
// Management key support
//

#[cfg(feature = "untested")]
#[test]
#[ignore]
fn test_set_mgmkey() {
    let mut yubikey = YUBIKEY.lock().unwrap();

    assert!(yubikey.verify_pin(b"123456").is_ok());

    fn test_mgm<M: MgmKeyAlgorithm>(yubikey: &mut YubiKey) {
        assert!(yubikey.authenticate::<M>(MgmKey::default()).is_ok());
        assert!(MgmKey::<M>::get_protected(yubikey).is_err());

        // Set a protected management key.
        assert!(MgmKey::<M>::generate().set_protected(yubikey).is_ok());
        let protected = MgmKey::<M>::get_protected(yubikey).unwrap();
        assert!(yubikey.authenticate::<M>(MgmKey::default()).is_err());
        assert!(yubikey.authenticate(protected.clone()).is_ok());

        // Set a manual management key.
        let manual = MgmKey::<M>::generate();
        assert!(manual.set_manual(yubikey, false).is_ok());
        assert!(MgmKey::<M>::get_protected(yubikey).is_err());
        assert!(yubikey.authenticate(MgmKey::<M>::default()).is_err());
        assert!(yubikey.authenticate(protected.clone()).is_err());
        assert!(yubikey.authenticate(manual.clone()).is_ok());

        // Set back to the default management key.
        assert!(MgmKey::<M>::set_default(yubikey).is_ok());
        assert!(MgmKey::<M>::get_protected(yubikey).is_err());
        assert!(yubikey.authenticate(protected).is_err());
        assert!(yubikey.authenticate(manual).is_err());
        assert!(yubikey.authenticate(MgmKey::<M>::default()).is_ok());
    }

    match get_mgm_key_meta(&mut yubikey).algorithm {
        ManagementAlgorithmId::ThreeDes => test_mgm::<des::TdesEee3>(&mut yubikey),
        ManagementAlgorithmId::Aes192 => test_mgm::<aes::Aes192>(&mut yubikey),
        ManagementAlgorithmId::Aes128 | ManagementAlgorithmId::Aes256 => {
            panic!("AES128 or AES256 should not be a default key")
        }
        other => panic!("unexpected management key algorithm: {:?}", other),
    }
}

//
// Certificate support
//

fn generate_self_signed_cert<KT: yubikey_signer::KeyType>() -> Certificate {
    let mut yubikey = YUBIKEY.lock().unwrap();

    assert!(yubikey.verify_pin(b"123456").is_ok());
    auth_default_mgm(&mut yubikey);

    let slot = SlotId::Retired(RetiredSlotId::R1);

    let generated = piv::generate(
        &mut yubikey,
        slot,
        KT::ALGORITHM,
        PinPolicy::Default,
        TouchPolicy::Default,
    )
    .unwrap();

    // 0x80 0x00 ... (20bytes) is invalid because of high MSB (serial will keep the sign)
    // we'll limit ourselves to 19 bytes serial.
    let mut serial = [0u8; 19];
    OsRng.fill_bytes(&mut serial);
    let serial = SerialNumber::new(&serial[..]).expect("serial can't be more than 20 bytes long");
    let validity = Validity::from_now(Duration::new(500000, 0)).unwrap();

    // Generate a self-signed certificate for the new key.
    let cert_result = Certificate::generate_self_signed::<_, KT>(
        &mut yubikey,
        slot,
        serial,
        validity,
        Name::from_str("CN=testSubject").expect("parse name"),
        generated,
        |_builder| Ok(()),
    );

    assert!(cert_result.is_ok());
    let cert = cert_result.unwrap();
    trace!("cert: {:?}", cert);
    cert
}

#[test]
#[ignore]
fn generate_self_signed_rsa_cert() {
    let cert = generate_self_signed_cert::<yubikey_signer::YubiRsa<yubikey_signer::Rsa1024>>();

    //
    // Verify that the certificate is signed correctly
    //

    let pubkey = RsaPublicKey::try_from(cert.subject_pki()).expect("valid rsa key");
    let pubkey = pkcs1v15::VerifyingKey::<Sha256>::new(pubkey);

    let data = cert.cert.to_der().expect("serialize certificate");
    let tbs_cert_len = u16::from_be_bytes(data[6..8].try_into().unwrap()) as usize;
    let msg = &data[4..8 + tbs_cert_len];
    let sig = pkcs1v15::Signature::try_from(&data[data.len() - 128..]).unwrap();
    let hash = Sha256::digest(msg);

    assert!(pubkey.verify_prehash(&hash, &sig).is_ok());
}

#[test]
#[ignore]
fn generate_self_signed_ec_cert() {
    let cert = generate_self_signed_cert::<p256::NistP256>();

    //
    // Verify that the certificate is signed correctly
    //

    let vk = p256::ecdsa::VerifyingKey::try_from(cert.subject_pki()).expect("ecdsa key expected");

    let data = cert.cert.to_der().expect("serialize certificate");
    let tbs_cert_len = data[6] as usize;
    let sig_algo_len = data[7 + tbs_cert_len + 1] as usize;
    let sig_start = 7 + tbs_cert_len + 2 + sig_algo_len + 3;
    let msg = &data[4..7 + tbs_cert_len];
    let sig = p256::ecdsa::Signature::from_der(&data[sig_start..]).unwrap();

    use p256::ecdsa::signature::Verifier;
    assert!(vk.verify(msg, &sig).is_ok());
}

#[test]
#[ignore]
fn test_slot_id_display() {
    assert_eq!(format!("{}", SlotId::Authentication), "Authentication");
    assert_eq!(format!("{}", SlotId::Signature), "Signature");
    assert_eq!(format!("{}", SlotId::KeyManagement), "KeyManagement");
    assert_eq!(
        format!("{}", SlotId::CardAuthentication),
        "CardAuthentication"
    );
    assert_eq!(format!("{}", SlotId::Attestation), "Attestation");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R1)), "R1");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R2)), "R2");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R3)), "R3");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R4)), "R4");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R5)), "R5");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R6)), "R6");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R7)), "R7");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R8)), "R8");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R9)), "R9");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R10)), "R10");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R11)), "R11");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R12)), "R12");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R13)), "R13");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R14)), "R14");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R15)), "R15");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R16)), "R16");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R17)), "R17");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R18)), "R18");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R19)), "R19");
    assert_eq!(format!("{}", SlotId::Retired(RetiredSlotId::R20)), "R20");

    assert_eq!(
        format!("{}", SlotId::Management(ManagementSlotId::Pin)),
        "Pin"
    );
    assert_eq!(
        format!("{}", SlotId::Management(ManagementSlotId::Puk)),
        "Puk"
    );
    assert_eq!(
        format!("{}", SlotId::Management(ManagementSlotId::Management)),
        "Management"
    );
}

//
// Metadata
//

#[test]
#[ignore]
fn test_read_metadata() {
    let mut yubikey = YUBIKEY.lock().unwrap();

    assert!(yubikey.verify_pin(b"123456").is_ok());
    auth_default_mgm(&mut yubikey);

    let slot = SlotId::Retired(RetiredSlotId::R1);

    // Generate a new key in the selected slot.
    let generated = piv::generate(
        &mut yubikey,
        slot,
        AlgorithmId::EccP256,
        PinPolicy::Default,
        TouchPolicy::Default,
    )
    .unwrap();

    match piv::metadata(&mut yubikey, slot) {
        Ok(metadata) => assert_eq!(metadata.public, Some(generated)),
        Err(Error::NotSupported) => {
            // Some YubiKeys don't support metadata
            eprintln!("metadata not supported by this YubiKey");
        }
        Err(err) => panic!("{}", err),
    }
}

#[test]
#[ignore]
fn test_read_metadata_missing_key() {
    let mut yubikey = YUBIKEY.lock().unwrap();

    assert!(yubikey.verify_pin(b"123456").is_ok());
    auth_default_mgm(&mut yubikey);

    // we assume that at least one of these slots is empty
    let slots_to_check = [
        RetiredSlotId::R10,
        RetiredSlotId::R11,
        RetiredSlotId::R12,
        RetiredSlotId::R13,
        RetiredSlotId::R14,
        RetiredSlotId::R15,
        RetiredSlotId::R16,
        RetiredSlotId::R17,
        RetiredSlotId::R18,
        RetiredSlotId::R19,
        RetiredSlotId::R20,
    ];

    for slot in slots_to_check {
        let slot = SlotId::Retired(slot);

        match piv::metadata(&mut yubikey, slot) {
            Ok(_) => {
                eprintln!("Key {} exists", slot);
            }
            Err(Error::NotSupported) => {
                // Some YubiKeys don't support metadata
                eprintln!("metadata not supported by this YubiKey");
                return;
            }
            Err(Error::NotFound) => {
                eprintln!("Key {} doesn't exist, ok.", slot);
                return;
            }
            Err(err) => panic!("{}", err),
        }
    }

    panic!("No empty slots to check");
}

#[test]
#[ignore]
fn test_parse_cert_from_der() {
    let bob_der = std::fs::read("tests/assets/Bob.der").expect(".der file not found");
    let cert = Certificate::from_bytes(bob_der).expect("Failed to parse valid certificate");
    assert_eq!(
        cert.subject(),
        "CN=Bob",
        "Subject is {} should be CN=Bob",
        cert.subject()
    );
    assert_eq!(
        cert.issuer(),
        "CN=Ferdinand Linnenberg CA",
        "Issuer is {} should be {}",
        cert.issuer(),
        "CN=Ferdinand Linnenberg CA"
    );
}
