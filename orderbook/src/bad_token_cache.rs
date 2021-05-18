use crate::bad_token::{BadTokenDetector, TokenQuality};
use anyhow::Result;
use primitive_types::H160;
use std::{collections::HashMap, sync::Mutex};

pub struct BadTokenCache {
    detector: BadTokenDetector,
    // Std mutex is fine because we don't hold lock across await.
    cache: Mutex<HashMap<H160, TokenQuality>>,
    // Explicitly allowed tokens that are always treated as good.
    allow_list: Vec<H160>,
}

impl BadTokenCache {
    pub fn new(detector: BadTokenDetector, allow_list: Vec<H160>) -> Self {
        Self {
            detector,
            cache: Default::default(),
            allow_list,
        }
    }

    pub async fn is_good(&self, token: H160) -> Result<bool> {
        if self.allow_list.contains(&token) {
            return Ok(true);
        }

        if let Some(info) = self.get_from_cache(&token) {
            return Ok(info.is_good());
        }

        match self.detector.detect(token).await {
            Ok(quality) => {
                let is_good = quality.is_good();
                tracing::info!("token {:?} quality {:?}", token, quality);
                self.insert_into_cache(token, quality);
                Ok(is_good)
            }
            Err(err) => {
                tracing::error!("token detector failed for token {:?}: {:?}", token, err);
                Err(err)
            }
        }
    }

    fn get_from_cache(&self, token: &H160) -> Option<TokenQuality> {
        self.cache.lock().unwrap().get(token).cloned()
    }

    fn insert_into_cache(&self, token: H160, quality: TokenQuality) {
        self.cache.lock().unwrap().insert(token, quality);
    }
}
