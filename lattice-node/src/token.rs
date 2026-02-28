use crate::Uuid;
use lattice_kernel::proto::network::InviteToken;
use lattice_model::types::PubKey;
use prost::Message;

/// Version byte for Lattice tokens (0x4C = 'L')
const TOKEN_VERSION: u8 = 0x4C;

/// Represents a parsed invite token
#[derive(Debug, Clone)]
pub struct Invite {
    pub inviter: PubKey,
    pub store_id: Uuid,
    pub secret: Vec<u8>,
}

impl Invite {
    /// Create a new invite
    pub fn new(inviter: PubKey, store_id: Uuid, secret: Vec<u8>) -> Self {
        Self {
            inviter,
            store_id,
            secret,
        }
    }

    /// Encode as Base58Check string with Lattice version byte
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        let proto = InviteToken {
            inviter_pubkey: self.inviter.to_vec(),
            store_id: self.store_id.as_bytes().to_vec(),
            secret: self.secret.clone(),
        };
        let bytes = proto.encode_to_vec();
        bs58::encode(bytes)
            .with_check_version(TOKEN_VERSION)
            .into_string()
    }

    /// Parse from Base58Check string with Lattice version byte
    pub fn parse(input: &str) -> Result<Self, String> {
        let bytes = bs58::decode(input)
            .with_check(Some(TOKEN_VERSION))
            .into_vec()
            .map_err(|e| format!("Invalid token: {}", e))?;

        // Skip version byte (first byte) when decoding protobuf
        let proto_bytes = bytes.get(1..).ok_or("Token too short")?;

        let token = InviteToken::decode(proto_bytes)
            .map_err(|e| format!("Protobuf decode failed: {}", e))?;

        let inviter = PubKey::try_from(token.inviter_pubkey.as_slice())
            .map_err(|_| "Invalid inviter pubkey length")?;

        let store_id = Uuid::from_slice(&token.store_id).map_err(|_| "Invalid store ID length")?;

        Ok(Self {
            inviter,
            store_id,
            secret: token.secret,
        })
    }
}
