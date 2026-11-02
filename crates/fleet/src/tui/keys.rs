use fleet_core::Config;
pub struct Keymap;
impl Keymap {
    pub fn from_config(_c: &Config) -> Self {
        Self
    }
    pub fn list(&self) -> Vec<(String, String)> {
        vec![]
    }
}
