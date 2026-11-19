//! Sealing a link, as [`shoal_protocol::hub::link`] describes: the X25519
//! exchange the handshake signs, the key it derives for each direction, and
//! the encryption of each numbered frame.
use anyhow::{Context as _, Result, anyhow, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    aead,
    agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519},
    hkdf,
    rand::SystemRandom,
};
use shoal_protocol::hub::{
    ExchangeKey,
    link::{HUB_TO_MEMBER, MEMBER_TO_HUB, Sealed, Signer, Transcript},
};

/// This side's ephemeral key for one link's exchange.
pub struct Exchange {
    private: EphemeralPrivateKey,
    pub public: ExchangeKey,
}

/// The keys of one link: of the frames this side sends, and of those it
/// receives.
pub struct Keys {
    pub sending: Sealer,
    pub receiving: Opener,
}

impl Exchange {
    pub fn new() -> Result<Self> {
        let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new())
            .map_err(|_| anyhow!("could not make an exchange key"))?;
        let public = private
            .compute_public_key()
            .map_err(|_| anyhow!("could not read an exchange key"))?;
        Ok(Self {
            public: ExchangeKey::try_from(public.as_ref())?,
            private,
        })
    }

    /// The keys of the link whose handshake `transcript` holds, for `side`.
    pub fn keys(self, transcript: &Transcript, side: Signer) -> Result<Keys> {
        let theirs = match side {
            Signer::Hub => &transcript.hello.exchange,
            Signer::Member => &transcript.hub_exchange,
        };
        let peer = UnparsedPublicKey::new(&X25519, theirs.as_bytes());
        let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, &transcript.salt());
        let [to_hub, to_member] = agreement::agree_ephemeral(self.private, &peer, |shared| {
            let secret = salt.extract(shared);
            [MEMBER_TO_HUB, HUB_TO_MEMBER].map(|info| derive(&secret, info))
        })
        .map_err(|_| anyhow!("the other side's exchange key is not a valid X25519 key"))?;
        let (sending, receiving) = match side {
            Signer::Hub => (to_member?, to_hub?),
            Signer::Member => (to_hub?, to_member?),
        };
        Ok(Keys {
            sending: Sealer {
                key: sending,
                next: 0,
            },
            receiving: Opener {
                key: receiving,
                next: 0,
            },
        })
    }
}

/// The key HKDF expands `secret` to with `info`.
fn derive(secret: &hkdf::Prk, info: &[u8]) -> Result<aead::LessSafeKey> {
    let info = [info];
    let okm = (secret.expand(&info, &aead::CHACHA20_POLY1305))
        .map_err(|_| anyhow!("could not derive a link key"))?;
    Ok(aead::LessSafeKey::new(aead::UnboundKey::from(okm)))
}

/// The nonce of frame `seq`: 4 zero bytes, then the number.
fn nonce(seq: u64) -> aead::Nonce {
    let mut nonce = [0; aead::NONCE_LEN];
    nonce[4..].copy_from_slice(&seq.to_be_bytes());
    aead::Nonce::assume_unique_for_key(nonce)
}

/// Seals the frames one side sends, in order.
pub struct Sealer {
    key: aead::LessSafeKey,
    next: u64,
}

impl Sealer {
    pub fn seal(&mut self, frame: String) -> Sealed {
        let seq = self.next;
        self.next += 1;
        let mut data = frame.into_bytes();
        (self
            .key
            .seal_in_place_append_tag(nonce(seq), aead::Aad::empty(), &mut data))
        .expect("ChaCha20-Poly1305 seals any frame a link takes");
        Sealed {
            seq,
            data: URL_SAFE_NO_PAD.encode(data),
        }
    }
}

/// Opens the frames one side receives, each once and in order.
pub struct Opener {
    key: aead::LessSafeKey,
    next: u64,
}

impl Opener {
    pub fn open(&mut self, sealed: Sealed) -> Result<String> {
        let Sealed { seq, data } = sealed;
        ensure!(
            seq == self.next,
            "a hub link frame came as number {seq}, not {}",
            self.next
        );
        let mut data = URL_SAFE_NO_PAD
            .decode(data)
            .context("a hub link frame is not base64url")?;
        let frame = (self
            .key
            .open_in_place(nonce(seq), aead::Aad::empty(), &mut data))
        .map_err(|_| anyhow!("a hub link frame does not open with the link's key"))?;
        let frame = String::from_utf8(frame.to_vec())?;
        self.next += 1;
        Ok(frame)
    }

    /// Whether it opened no frame yet.
    pub fn fresh(&self) -> bool {
        self.next == 0
    }
}

#[cfg(test)]
mod tests;
