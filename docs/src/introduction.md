# Introduction

Resysto is a multi-tenant GRC platform. Companies use it to run governance, risk and compliance for their security programme, and managed security service providers use it to do the same work for all of their customers from one place.

The stack:

- **Backend**: Rust, with [Axum](https://github.com/tokio-rs/axum) for HTTP.
- **Database**: [SurrealDB](https://surrealdb.com) 4 on the SurrealKV engine, one database per tenant.
- **Background work**: [Restate](https://restate.dev) for durable jobs and workflows.
- **Frontend**: [Svelte](https://svelte.dev) 5 with SvelteKit, built as a single-page app and served by the backend.

This guide is for everyone who writes code for resysto: the developers on the team and the coding agents that work alongside them. It explains how the codebase is organized and which patterns we use, and why, so you know where a new piece of code belongs before you write it.
