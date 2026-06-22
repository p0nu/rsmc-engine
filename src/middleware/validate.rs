//! A `Json` extractor that also runs `validator` validation.

use crate::error::AppError;
use axum::{
    extract::{rejection::JsonRejection, FromRequest, Request},
    Json,
};
use validator::Validate;

/// Like [`axum::Json`] but validates the body via the [`Validate`] trait.
pub struct ValidatedJson<T>(pub T);

#[async_trait::async_trait]
impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: serde::de::DeserializeOwned + Validate,
    S: Send + Sync,
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?;
        value.validate()?;
        Ok(ValidatedJson(value))
    }
}
