//! The HTTPS client shared by the commands that fetch from the network.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use rustls_pki_types::{
    AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm, alg_id,
};

/// An agent that speaks HTTPS through rustls with the pure-Rust crypto
/// provider, which also verifies certificates signed with RSA (see
/// [`RsaPkcs1`]), giving up on a request after `timeout` in all.
pub fn agent(timeout: Duration) -> ureq::Agent {
    let mut provider = rustls_pure::provider();
    provider.signature_verification_algorithms.all = CERTIFICATE_SIGNATURES.as_slice();
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .unversioned_rustls_crypto_provider(Arc::new(provider))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(timeout))
        .build()
        .new_agent()
}

/// The signatures a certificate chain may be signed with: rustls-pure's
/// (ECDSA and Ed25519), and RSA PKCS#1 v1.5 with SHA-256, SHA-384 or
/// SHA-512, with which certificate authorities still sign many sites'
/// certificates, Yahoo's among them.
static CERTIFICATE_SIGNATURES: LazyLock<Vec<&'static dyn SignatureVerificationAlgorithm>> =
    LazyLock::new(|| {
        let rsa: [&'static dyn SignatureVerificationAlgorithm; 3] =
            [&RSA_PKCS1_SHA256, &RSA_PKCS1_SHA384, &RSA_PKCS1_SHA512];
        let pure = rustls_pure::provider().signature_verification_algorithms;
        [pure.all, &rsa].concat()
    });

/// The verification of RSA PKCS#1 v1.5 signatures with one hash, by a key of
/// 2048 to 8192 bits, for certificates signed with RSA. rustls-pure leaves
/// RSA out for the timing side channel of the rsa crate (RUSTSEC-2023-0071),
/// which is in decrypting and signing with a private key, and is fixed in
/// rsa 0.10; this uses only public keys. Limits: a server whose own key is
/// RSA is still refused, since TLS 1.3 has it sign the handshake with
/// RSA-PSS, which is not added, and there is no RSA key exchange.
#[derive(Debug)]
struct RsaPkcs1 {
    signature_alg_id: AlgorithmIdentifier,
    verify: fn(&rsa::PublicKey, &[u8], &[u8]) -> rsa::Result<()>,
}

static RSA_PKCS1_SHA256: RsaPkcs1 = RsaPkcs1 {
    signature_alg_id: alg_id::RSA_PKCS1_SHA256,
    verify: rsa::PublicKey::pkcs1v15_verify::<rsa::sha2::Sha256>,
};

static RSA_PKCS1_SHA384: RsaPkcs1 = RsaPkcs1 {
    signature_alg_id: alg_id::RSA_PKCS1_SHA384,
    verify: rsa::PublicKey::pkcs1v15_verify::<rsa::sha2::Sha384>,
};

static RSA_PKCS1_SHA512: RsaPkcs1 = RsaPkcs1 {
    signature_alg_id: alg_id::RSA_PKCS1_SHA512,
    verify: rsa::PublicKey::pkcs1v15_verify::<rsa::sha2::Sha512>,
};

impl SignatureVerificationAlgorithm for RsaPkcs1 {
    fn verify_signature(
        &self,
        public_key: &[u8],
        message: &[u8],
        signature: &[u8],
    ) -> Result<(), InvalidSignature> {
        let (n, e) = rsa_public_key(public_key).ok_or(InvalidSignature)?;
        let key = rsa::PublicKey::new_with_size_limits(
            rsa::BoxedUint::from_be_slice_vartime(n),
            rsa::BoxedUint::from_be_slice_vartime(e),
            2048..=8192,
        )
        .map_err(|_| InvalidSignature)?;
        (self.verify)(&key, message, signature).map_err(|_| InvalidSignature)
    }

    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }

    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.signature_alg_id
    }
}

/// The modulus and public exponent of an RSAPublicKey in DER: a sequence of
/// two positive integers, with nothing after it.
fn rsa_public_key(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let (key, rest) = der_value(der, 0x30)?;
    let (n, key) = der_value(key, 0x02)?;
    let (e, key) = der_value(key, 0x02)?;
    let positive = |i: &[u8]| i.first().is_some_and(|b| b & 0x80 == 0);
    (rest.is_empty() && key.is_empty() && positive(n) && positive(e)).then_some((n, e))
}

/// The value of the DER element at the start of `der` if it has this tag,
/// its length written in one, two or three bytes, and what follows it.
fn der_value(der: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let [t, len, rest @ ..] = der else {
        return None;
    };
    let (len, rest) = match *len {
        n @ 0..=0x7f => (usize::from(n), rest),
        0x81 => (usize::from(*rest.first()?), &rest[1..]),
        0x82 => {
            let (n, rest) = rest.split_first_chunk::<2>()?;
            (usize::from(u16::from_be_bytes(*n)), rest)
        }
        _ => return None,
    };
    if *t != tag {
        return None;
    }
    rest.split_at_checked(len)
}

/// Serve one HTTP response on a local port, giving the server's base URL.
#[cfg(test)]
pub(crate) fn serve_once(status: &'static str, body: &'static str) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
            line.clear();
        }
        let mut stream = stream;
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    format!("http://{addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes written in hexadecimal, white space ignored.
    fn hex(s: &str) -> Vec<u8> {
        let digits: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        (digits.chunks(2))
            .map(|d| u8::from_str_radix(std::str::from_utf8(d).unwrap(), 16).unwrap())
            .collect()
    }

    const MESSAGE: &[u8] = b"hledgers checks certificates signed with RSA";

    #[test]
    fn rsa_pkcs1_signatures() {
        let key = hex(KEY_2048);
        let algorithms = [
            (&RSA_PKCS1_SHA256, SIG_SHA256),
            (&RSA_PKCS1_SHA384, SIG_SHA384),
            (&RSA_PKCS1_SHA512, SIG_SHA512),
        ];
        for (rsa, sig) in algorithms {
            let sig = hex(sig);
            assert!(rsa.verify_signature(&key, MESSAGE, &sig).is_ok());
            // another message, or the signature with a bit changed, fails
            let other = b"hledgers checks certificates signed with RSA.";
            assert!(rsa.verify_signature(&key, other, &sig).is_err());
            let mut changed = sig.clone();
            changed[100] ^= 1;
            assert!(rsa.verify_signature(&key, MESSAGE, &changed).is_err());
        }
        // a signature made with another hash fails
        let sig = hex(SIG_SHA256);
        assert!(
            RSA_PKCS1_SHA384
                .verify_signature(&key, MESSAGE, &sig)
                .is_err()
        );
        // and so does a key that is not an RSAPublicKey
        let cut = &key[..key.len() - 1];
        assert!(
            RSA_PKCS1_SHA256
                .verify_signature(cut, MESSAGE, &sig)
                .is_err()
        );
    }

    #[test]
    fn keys_under_2048_bits_are_refused() {
        let (key, sig) = (hex(KEY_1024), hex(SIG_1024));
        assert!(
            RSA_PKCS1_SHA256
                .verify_signature(&key, MESSAGE, &sig)
                .is_err()
        );
        // though the signature is good
        let (n, e) = rsa_public_key(&key).unwrap();
        let small = rsa::PublicKey::new_with_size_limits(
            rsa::BoxedUint::from_be_slice_vartime(n),
            rsa::BoxedUint::from_be_slice_vartime(e),
            1024..=8192,
        )
        .unwrap();
        assert!(
            small
                .pkcs1v15_verify::<rsa::sha2::Sha256>(MESSAGE, &sig)
                .is_ok()
        );
    }

    #[test]
    fn rsa_public_keys_in_der() {
        let key = hex(KEY_2048);
        let (n, e) = rsa_public_key(&key).unwrap();
        assert_eq!((n.len(), e), (257, &[1, 0, 1][..]));
        // cut short, with a byte more, not a sequence, a negative integer
        assert_eq!(rsa_public_key(&key[..key.len() - 1]), None);
        assert_eq!(rsa_public_key(&[&key[..], &[0]].concat()), None);
        assert_eq!(rsa_public_key(&hex("3106 020103 020103")), None);
        assert_eq!(rsa_public_key(&hex("3006 020181 020103")), None);
        assert_eq!(
            rsa_public_key(&hex("3006 020103 020103")),
            Some((&[3][..], &[3][..]))
        );
    }

    #[test]
    fn a_certificate_in_yahoos_chain() {
        // DigiCert Global G2 TLS RSA SHA256 2020 CA1, which signs Yahoo's
        // certificate, as signed by DigiCert Global Root G2
        let (key, tbs, sig) = (hex(ROOT_KEY), hex(CA_TBS), hex(CA_SIG));
        assert!(RSA_PKCS1_SHA256.verify_signature(&key, &tbs, &sig).is_ok());
        assert!(
            RSA_PKCS1_SHA256
                .verify_signature(&key, &tbs[1..], &sig)
                .is_err()
        );
    }

    // An RSA-2048 public key, an RSAPublicKey in DER, and its PKCS#1 v1.5
    // signatures of MESSAGE with SHA-256, SHA-384 and SHA-512, made with
    // openssl (genpkey, rsa -RSAPublicKey_out, dgst -sign).
    const KEY_2048: &str = "
        3082010a0282010100af5d993b31e59027b557a4ef009bb772e10b98c4b661906190d1edb4331265
        01ace832d376a481a5d80c0364ed9fed9b1228a5ee9d8aba8d5ebcd24931a57535e213a46851423a
        f0cca3eb46abc533b6d4d3319f9d2b35402bf000fc10581945b1a944d38046d45459bc8abab1e3a4
        8a6bc48408e59cec3aca783c17593323548d8f5c79753c5cccc2a4c6e5634f6e5069fd5b766a7da4
        88ee6688f002731ea0c224e8af285ccac94486ab847f2ae7f819e18e4e83552fee14bd9b4bc1ca77
        a0700ae591e1ba29c5caf534402dfeaeaf155218b9090e2c7fdc3a8651e75e0aba001cd93ff1a915
        5d290e87889d6582978907593164322881365a9604323d9f150203010001
    ";

    const SIG_SHA256: &str = "
        197523ce03fa0f88279e663d5a01c3bd03b32627670f095ca07a393a468a462d0e1bec14510c47fa
        cee780c59d256a478cc9b1e7c17bc970c434c5b05484e661335981b0048b1a244ed2054e918e4ee6
        a68a594fe1cc139ba205493930deef501fb99bdd87762064f7a70c1f59139d31de04be1c6fcc6096
        8c6881682d074a3fd77b630448f04608f0bc09fc97bae2b9242845e8be5d4e54baff89338173daa2
        be7c5c3d2fd50e2e98019d80ce022176b18d8bdb77d5c89980fffdeb928318d36cb82f5f897b3bb4
        81d8f93251b00d2bdeb3370e87374af2c0ad24a26b423fa32407932a50de24341562b14548708cfb
        533861c6f21112d98d7a986e3708f703
    ";

    const SIG_SHA384: &str = "
        7f29cb7dd3f437699b163275665c92b152697285697d017d88e7089da9f3111860a15d4f54375f12
        78947747ae94093779c3e7b5e16dbdb755928dcf297f138905e691c4adbeeaa3711789d7c8ed6223
        944952f7e2418119a4e6046cc481c0ea4f12a6249845588269fc4eb215069b30ac80e05390108d8d
        a25693ef72382e2a6707e6f7c4413f02e41f7326542cf8f7c0e092594eab543ab967920708fd3cd4
        9eb9c40f86e2ad0330c9efc0cf6e46137d37c322e1662e3ef03132cd85c00964dfd2e58e4d2ae08b
        2c9f183ed7e1867f79472d5302f6426b454c13bcb00c8faf610b4b62e365fb3773e0bf6fa9850752
        270c51be517c8b2502e99fb5661eccce
    ";

    const SIG_SHA512: &str = "
        46783fed14c11b5044d1917454662e1edcb344295e2c85b35f1c10602135fdf136970ab39cf6113c
        3db5b7cefc112c9cd8183a30a42f6c29482a84f02434c0c889a23794d565437b14f4bcdebc895571
        d70d5a039a69366460964435fcce701fede12014a1b94ca13ef8ca25cb1731b6cbc6a27dcdb8a123
        2e84d963c6be8178a15547e6107ee85986166bb426280809c0fe5f7b20f171f2bb654b26975b7f29
        a7cacdcc7fad24b1c7fb6f1f4b9b283abe8d2ccbf005b8a36cccac534c05b1a67d2558686849ab21
        c3c9b5193b75a087b3d83f0f0f19c4033d24360c105fdb351629db332a56c12d2f92be1068ba8ec0
        082891fc2c2f55d25ad1137766c54d6d
    ";

    // An RSA-1024 public key, and its SHA-256 signature of MESSAGE.
    const KEY_1024: &str = "
        30818902818100b662c957bd20b4d878e89dcfadc5dd976e134a3170fe45a776f091b6af2c67bf53
        87697a93c860f8f86ffc19bf259579b8c608081cd06d01d79d11d26de3b2946a6919a53b35264c8a
        ec13032bec00f58fff090342fe5381c99e046e1403f7d633968b2d179aff64c1d58343f7e4e97406
        2283a480e35efc299b5ce8c02f72c90203010001
    ";

    const SIG_1024: &str = "
        869e89e8415b0c1b55e200bf938ec9ba00739f4fe647a5481fcc21f21efc3d6e1d62efbfbfc1df87
        f4c95904ab1c50fe4997c28f1fed3a615d912dcbdde0d399d1b6101e37bc53294ac9be5762638711
        355e1eae24517efce2fc2f387dbc51367ba0a7921d549731d66b12156c40032a34df1d0ce163da22
        955bd92f1dde5c9c
    ";

    // DigiCert Global Root G2's key, the tbsCertificate of the certificate it
    // signed for DigiCert Global G2 TLS RSA SHA256 2020 CA1, and that
    // certificate's sha256WithRSAEncryption signature.
    const ROOT_KEY: &str = "
        3082010a0282010100bb37cd34dc7b6bc9b26890ad4a75ff46ba210a088df51954c9fb88dbf3aef2
        3a89913c7ae6ab061a6bcfac2de85e092444ba629a7ed6a3a87ee054752005ac50b79c631a6c30dc
        da1f19b1d71edefdd7e0cb948337aeec1f434edd7b2cd2bd2ea52fe4a9b8ad3ad499a4b625e99b6b
        00609260ff4f214918f76790ab61069c8ff2bae9b4e992326bb5f357e85d1bcd8c1dab95049549f3
        352d96e3496ddd77e3fb494bb4ac5507a98f95b3b423bb4c6d45f0f6a9b29530b4fd4c558c274a57
        147c829dcd7392d3164a060c8c50d18f1e09be17a1e621cafd83e510bc83a50ac46728f67314143d
        4676c387148921344daf0f450ca649a1babb9cc5b1338329850203010001
    ";

    const CA_TBS: &str = "
        308203dca0030201020210085f94c02d857be8cc14ff53eda23e2a300d06092a864886f70d01010b
        05003061310b300906035504061302555331153013060355040a130c446967694365727420496e63
        31193017060355040b13107777772e64696769636572742e636f6d3120301e060355040313174469
        67694365727420476c6f62616c20526f6f74204732301e170d3230303932343030303030305a170d
        3330303932333233353935395a3059310b300906035504061302555331153013060355040a130c44
        6967694365727420496e63313330310603550403132a446967694365727420476c6f62616c204732
        20544c53205253412053484132353620323032302043413130820122300d06092a864886f70d0101
        0105000382010f003082010a0282010100ccf710624fa6bb636fed905256c56d277b7a12568af1f4
        f9d6e7e18fbd95abf260411570db1200fa270ab557385b7db2519371950e6a41945b351bfa7bfabb
        c5be2430fe56efc4f37d97e314f5144dcba710f216eaab22f031221161699026ba78d9971fe37d66
        ab75449573c8acffef5d0a8a5943e1acb23a0ff348fcd76b37c163dcde46d6db45fe7d23fd90e851
        071e51a35fed4946547f2c88c5f4139c97153c03e8a139dc690c32c1af16574c9447427ca2c89c7d
        e6d44d54af4299a8c104c2779cd648e4ce11e02a8099f04370cf3f766bd14c49ab245ec20d82fd46
        a8ab6c93cc6252427592f89afa5e5eb2b061e51f1fb97f0998e83dfa837f4769a10203010001a382
        01ae308201aa301d0603551d0e04160414748580c066c7df37decfbd2937aa031dbeedcd17301f06
        03551d230418301680144e2254201895e6e36ee60ffafab912ed06178f39300e0603551d0f0101ff
        040403020186301d0603551d250416301406082b0601050507030106082b06010505070302301206
        03551d130101ff040830060101ff020100307606082b06010505070101046a3068302406082b0601
        05050730018618687474703a2f2f6f6373702e64696769636572742e636f6d304006082b06010505
        0730028634687474703a2f2f636163657274732e64696769636572742e636f6d2f44696769436572
        74476c6f62616c526f6f7447322e637274307b0603551d1f047430723037a035a033863168747470
        3a2f2f63726c332e64696769636572742e636f6d2f4469676943657274476c6f62616c526f6f7447
        322e63726c3037a035a0338631687474703a2f2f63726c342e64696769636572742e636f6d2f4469
        676943657274476c6f62616c526f6f7447322e63726c30300603551d20042930273007060567810c
        01013008060667810c0102013008060667810c0102023008060667810c010203
    ";

    const CA_SIG: &str = "
        758bc03c5befff705c6b8af521efac31e6433d80afd755a91ec38e186c79d3f7ac42df8f88c59db4
        75f3c908fd8afe51f3f29d201a0435cef8b64179afffcc965e94d39a1d702ea2318bfba5cc7f56df
        173797bfc18d4386ab8c667eb9ed8bf10f8cb5d455af5c899ee0a72a6db2a36332183bcbdeb59f83
        b31101ac200d909712b7299c35b5d7910eef406f6d5c3bbda1d9fbe2cb2e7640d23abb2cbf160bea
        bd035b4e983a2aed05500715933c733991945a480ec6d34a01af6639b9bd42341cdb0803f340fc12
        e8ae3cab8ad5fb727964a6e30e55b13b953fad6033678aacb6a75cf0ed5a521bd461e12512e607b5
        aa94b2f028f661265e551cf801eb7771
    ";
}
