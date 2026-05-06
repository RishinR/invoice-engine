use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub psp_url: String,
    pub psp_timeout_secs: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        dotenvy::dotenv().ok();
        Ok(Config {
            database_url: env::var("DATABASE_URL")
                .map_err(|_| "DATABASE_URL must be set")?,
            port: env::var("PORT")
                .unwrap_or_else(|_| "3000".into())
                .parse()
                .map_err(|_| "PORT must be a valid u16")?,
            psp_url: env::var("PSP_URL")
                .unwrap_or_else(|_| "http://localhost:4000".into()),
            psp_timeout_secs: env::var("PSP_TIMEOUT_SECS")
                .unwrap_or_else(|_| "10".into())
                .parse()
                .map_err(|_| "PSP_TIMEOUT_SECS must be a number")?,
        })
    }
}
