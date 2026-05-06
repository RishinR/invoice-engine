use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use uuid::Uuid;

use crate::{
    auth::AuthBusiness,
    error::AppError,
    models::customer::{CreateCustomerRequest, Customer},
    state::AppState,
};

pub async fn create(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Json(req): Json<CreateCustomerRequest>,
) -> Result<(StatusCode, Json<Customer>), AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError::Unprocessable("name is required".into()));
    }
    if req.email.trim().is_empty() || !req.email.contains('@') {
        return Err(AppError::Unprocessable("valid email is required".into()));
    }

    let customer = sqlx::query_as::<_, Customer>(
        r#"INSERT INTO customers (business_id, name, email)
           VALUES ($1, $2, $3)
           RETURNING *"#,
    )
    .bind(auth.business_id)
    .bind(req.name.trim())
    .bind(req.email.trim().to_lowercase())
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.code().as_deref() == Some("23505") {
                return AppError::Conflict("A customer with this email already exists".into());
            }
        }
        AppError::Database(e)
    })?;

    Ok((StatusCode::CREATED, Json(customer)))
}

pub async fn list(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
) -> Result<Json<Vec<Customer>>, AppError> {
    let customers = sqlx::query_as::<_, Customer>(
        "SELECT * FROM customers WHERE business_id = $1 ORDER BY created_at DESC",
    )
    .bind(auth.business_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(customers))
}

pub async fn get_by_id(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthBusiness>,
    Path(id): Path<Uuid>,
) -> Result<Json<Customer>, AppError> {
    let customer = sqlx::query_as::<_, Customer>(
        "SELECT * FROM customers WHERE id = $1 AND business_id = $2",
    )
    .bind(id)
    .bind(auth.business_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Customer {id} not found")))?;

    Ok(Json(customer))
}
