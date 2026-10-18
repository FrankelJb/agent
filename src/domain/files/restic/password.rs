use crate::utils::edge_key::derive_key;
use anyhow::Result;

pub fn derive_password(master_key_b64: &str, generated_id: &str) -> Result<String> {
    let info = format!("portabase/restic/v1/{generated_id}");
    Ok(hex::encode(derive_key(master_key_b64, info.as_bytes())?))
}
