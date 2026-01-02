use crate::types::PubKey;
use crate::Uuid;
use crate::proto::network::InviteToken;
use prost::Message;

/// Version byte for Lattice tokens (0x4C = 'L')
const TOKEN_VERSION: u8 = 0x4C;

/// Represents a parsed invite token
#[derive(Debug, Clone)]
pub struct Invite {
    pub inviter: PubKey,
    pub mesh_id: Uuid,
    pub secret: Vec<u8>,
}

impl Invite {
    /// Create a new invite
    pub fn new(inviter: PubKey, mesh_id: Uuid, secret: Vec<u8>) -> Self {
        Self { inviter, mesh_id, secret }
    }

    /// Encode as Base58Check string with Lattice version byte
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        let proto = InviteToken {
            inviter_pubkey: self.inviter.to_vec(),
            mesh_id: self.mesh_id.as_bytes().to_vec(),
            secret: self.secret.clone(),
        };
        let bytes = proto.encode_to_vec();
        bs58::encode(bytes).with_check_version(TOKEN_VERSION).into_string()
    }

    /// Parse from Base58Check string with Lattice version byte
    pub fn parse(input: &str) -> Result<Self, String> {
        let bytes = bs58::decode(input).with_check(Some(TOKEN_VERSION)).into_vec()
            .map_err(|e| format!("Invalid token: {}", e))?;
            
        let token = InviteToken::decode(bytes.as_slice())
            .map_err(|e| format!("Protobuf decode failed: {}", e))?;
            
        let inviter = PubKey::try_from(token.inviter_pubkey.as_slice())
            .map_err(|_| "Invalid inviter pubkey length")?;
            
        let mesh_id = Uuid::from_slice(&token.mesh_id)
            .map_err(|_| "Invalid mesh ID length")?;
            
        Ok(Self {
            inviter,
            mesh_id,
            secret: token.secret,
        })
    }
}
