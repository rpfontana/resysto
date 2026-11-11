# Request Validation

Client-side validation is a nice UX touch, but it's not a security boundary. Someone with curl doesn't care about the checks in our Svelte forms. We always validate on the server.

Validation splits into two levels. At the API layer, we check the shape and format of what comes in: are the required fields present? Does the email field actually look like an email? Is the password long enough? Then at the domain layer, we enforce business invariants through the type system: values with rules, like `Email`, are domain types that can only be built by parsing (parse, don't validate).

This chapter covers that first level, the API-layer validation, with the `validator` crate and a custom Axum extractor.

## Validation with the validator crate

The `validator` crate gives us derive macros to annotate struct fields with validation rules. You call `.validate()` on an instance, it checks every rule, and hands back a structured error if anything fails:

```rust
use serde::Deserialize;
use validator::Validate;

#[derive(Debug, Deserialize, Validate)]
pub struct CreateUserDto {
    #[validate(length(min = 2, max = 50, message = "name must be between 2 and 50 characters"))]
    pub name: String,

    #[validate(email(message = "must be a valid email address"))]
    pub email: String,

    #[validate(length(min = 8, message = "password must be at least 8 characters"))]
    pub password: String,
}

#[derive(Debug, Deserialize, Validate)]
pub struct UpdateUserDto {
    #[validate(length(min = 2, max = 50, message = "name must be between 2 and 50 characters"))]
    pub name: Option<String>,

    #[validate(length(max = 500, message = "bio cannot exceed 500 characters"))]
    pub bio: Option<String>,

    #[validate(url(message = "must be a valid URL"))]
    pub avatar_url: Option<String>,
}
```

The `validator` crate comes with a solid set of built-in validations: `email`, `url`, `length`, `range`, `contains`, `must_match` (handy for password confirmation), and more. Anything that doesn't fit the built-in validators gets a custom validation function.

## A custom ValidatedJson extractor

Axum's built-in `Json` extractor handles deserialization, but it doesn't run any validation. Calling `.validate()` by hand at the start of every handler gets old fast and it's easy to forget in one place. So we use a custom extractor that combines deserialization and validation into a single step.

```rust
use std::collections::HashMap;
use axum::{
    extract::{FromRequest, Request},
    Json,
};
use validator::Validate;

pub struct ValidatedJson<T>(pub T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: serde::de::DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let Json(value) = Json::<T>::from_request(req, state)
            .await
            .map_err(|e| AppError::Validation(format!("invalid JSON: {e}")))?;

        value.validate().map_err(|e| {
            AppError::ValidationFields(format_validation_errors(&e))
        })?;

        Ok(Self(value))
    }
}

fn format_validation_errors(
    errors: &validator::ValidationErrors,
) -> HashMap<String, Vec<String>> {
    errors
        .field_errors()
        .iter()
        .map(|(field, errs)| {
            let messages = errs
                .iter()
                .map(|e| {
                    e.message
                        .as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| format!("{} is invalid", field))
                })
                .collect();
            (field.to_string(), messages)
        })
        .collect()
}
```

Handlers accept `ValidatedJson<CreateUserDto>` instead of `Json<CreateUserDto>`, and validation happens before the handler body ever runs. If validation fails, the client gets a 400 response with a clear error message telling it exactly which fields failed and why.

```rust
async fn create_user(
    State(state): State<AppState>,
    ValidatedJson(payload): ValidatedJson<CreateUserDto>,
) -> AppResult<(StatusCode, Json<UserResponse>)> {
    // By this point, we know:
    // - The JSON was well-formed
    // - name is between 2 and 50 characters
    // - email is a valid email format
    // - password is at least 8 characters
    //
    // The handler can focus on business logic.
    let user = state.user_service.register(payload).await?;
    Ok((StatusCode::CREATED, Json(user.into())))
}
```

## Validating query parameters

The same approach works for query parameters, with a `ValidatedQuery` extractor that combines `Query` extraction with validation:

```rust
use axum::extract::Query;

pub struct ValidatedQuery<T>(pub T);

impl<T, S> FromRequestParts<S> for ValidatedQuery<T>
where
    T: serde::de::DeserializeOwned + Validate,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|e| AppError::Validation(format!("invalid query parameters: {e}")))?;

        value.validate().map_err(|e| {
            AppError::ValidationFields(format_validation_errors(&e))
        })?;

        Ok(Self(value))
    }
}
```

## Structured error responses

Validation errors go back in a structured format the client can parse, not a single concatenated string: a JSON object with per-field error lists, so the UI can show each message next to its field.

```json
{
    "error": {
        "type": "validation_error",
        "message": "request validation failed",
        "fields": {
            "email": ["must be a valid email address"],
            "password": ["password must be at least 8 characters"]
        }
    }
}
```

This is the `ValidationFields` variant of `AppError` from the [Error Handling](../core-patterns/error-handling.md) chapter. The idea is to give the client enough information to fix everything in one shot. Nobody wants to fix one field, resubmit, and then discover the next failure.

## Pre-built validation extractors

The ecosystem has ready-made extractors:

- **axum-valid** integrates with `validator`, `garde`, and `validify`, providing `Valid<Json<T>>`, `Valid<Query<T>>`, and `Valid<Form<T>>` extractors
- **axum-validated-extractors** provides `ValidatedJson`, `ValidatedQuery`, and `ValidatedForm` out of the box

They save the boilerplate we wrote above. We roll our own anyway, because it gives us full control over the error format and behavior.

## Validation at two levels

The distinction between API-layer and domain-layer validation matters, because mixing them up leads to either duplicated logic or gaps in protection.

**API-layer validation** (what this chapter covers) checks the shape of the input: are required fields present? Are strings the right length? Does the email field actually look like an email? This is about the format of data as it arrives over the wire.

**Domain-layer validation** enforces business invariants: a username doesn't contain forbidden characters, an email gets normalized to lowercase, an order amount is positive. This validation happens when we construct domain types like `UserName::parse()` or `Email::parse()`, which is the only way to construct them.

These two levels work together. The API layer catches obviously malformed input early and returns a helpful error message. The domain layer makes sure business rules are enforced consistently, whether the data comes from an HTTP request, a Restate job, a CSV import, or a test.
