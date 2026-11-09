-- idempotent import
USE NS `resysto-dev`;
DEFINE USER IF NOT EXISTS ns_migrator ON NAMESPACE PASSWORD 'migrator' ROLES OWNER;
