pub mod customers;
pub mod invoices;
pub mod payments;
pub mod webhook_endpoints;

use axum::{
    middleware,
    routing::{get, post},
    Router,
};

use crate::{auth, state::AppState};

pub fn build(state: AppState) -> Router {
    Router::new()
        // Customers
        .route("/customers", post(customers::create).get(customers::list))
        .route("/customers/{id}", get(customers::get_by_id))
        // Invoices
        .route("/invoices", post(invoices::create).get(invoices::list))
        .route("/invoices/{id}", get(invoices::get_by_id))
        .route("/invoices/{id}/open", post(invoices::open_invoice))
        .route("/invoices/{id}/void", post(invoices::void_invoice))
        .route(
            "/invoices/{id}/mark-uncollectible",
            post(invoices::mark_uncollectible),
        )
        // Payments
        .route("/invoices/{id}/pay", post(payments::pay))
        // Webhooks
        .route(
            "/webhook-endpoints",
            post(webhook_endpoints::create).get(webhook_endpoints::list),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
        .with_state(state)
}
