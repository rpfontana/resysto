-- what happened to accounts, sessions and tenants, written by the server
DEFINE TABLE OVERWRITE audit_event SCHEMAFULL PERMISSIONS NONE;

-- ------------------------------
-- FIELDS
-- ------------------------------

-- no reference: the row outlives the account
DEFINE FIELD OVERWRITE account ON audit_event TYPE option<record<account>>;
DEFINE FIELD OVERWRITE at ON audit_event TYPE datetime DEFAULT time::now();
DEFINE FIELD OVERWRITE event ON audit_event TYPE
    'account_created' | 'member_added' | 'login' | 'login_failed' | 'second_factor_refused'
    | 'recovery_code_used' | 'logout' | 'password_changed' | 'password_refused'
    | 'session_ended' | 'other_sessions_ended' | 'totp_bound' | 'totp_unbound'
    | 'totp_reset' | 'mfa_requirement_changed' | 'unlocked';
-- the address the request came from, as the server saw it
DEFINE FIELD OVERWRITE ip ON audit_event TYPE option<string>;
-- the key of the session the event happened in, or ended
DEFINE FIELD OVERWRITE session ON audit_event TYPE option<string>;
DEFINE FIELD OVERWRITE tenant ON audit_event TYPE option<record<tenant>>;
DEFINE FIELD OVERWRITE user_agent ON audit_event TYPE option<string>;
