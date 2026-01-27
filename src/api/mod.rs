//! HTTP API Handlers and Routes
//!
//! This module provides the REST API layer for A.R.E.S, built on the Axum web framework.
//!
//! # Module Structure
//!
//! - [`api::handlers`](crate::api::handlers) - Request handlers for each endpoint
//! - [`api::routes`](crate::api::routes) - Route definitions and router configuration
//!
//! # API Endpoints
//!
//! ## Authentication (`/api/auth`)
//! - `POST /api/auth/register` - Register new user
//! - `POST /api/auth/login` - Login and receive JWT token
//!
//! ## Chat (`/api/chat`)
//! - `POST /api/chat` - Send message and receive streaming response
//! - `GET /api/memory` - Get user memory (facts, preferences)
//!
//! ## Conversations (`/api/conversations`)
//! - `GET /api/conversations` - List user's conversations
//! - `GET /api/conversations/{id}` - Get conversation with messages
//! - `PUT /api/conversations/{id}` - Update conversation title
//! - `DELETE /api/conversations/{id}` - Delete conversation
//!
//! ## RAG (`/api/rag`)
//! - `POST /api/rag/ingest` - Ingest documents into a collection
//! - `POST /api/rag/search` - Search for relevant documents
//! - `GET /api/rag/collections` - List collections
//! - `DELETE /api/rag/collections/{name}` - Delete a collection
//!
//! ## Health (`/api/health`)
//! - `GET /api/health` - Health check endpoint
//!
//! # Authentication
//!
//! Most endpoints require a valid JWT token in the `Authorization` header:
//! ```text
//! Authorization: Bearer <token>
//! ```
//!
//! # OpenAPI Documentation
//!
//! When the `swagger-ui` feature is enabled, interactive API documentation
//! is available at `/swagger-ui/`.

/// Request and response handlers for all API endpoints.
pub mod handlers;
/// Router configuration and route definitions.
pub mod routes;
