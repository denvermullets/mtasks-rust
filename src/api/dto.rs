use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct TeamDto {
    #[allow(dead_code)]
    pub id: i64,
}
