-- Imported on every start: every statement must be idempotent.
DEFINE NAMESPACE IF NOT EXISTS `resysto-dev`;
USE NS `resysto-dev`;
DEFINE USER IF NOT EXISTS ns_migrator ON NAMESPACE PASSWORD 'migrator' ROLES OWNER;
DEFINE USER IF NOT EXISTS ns_app ON NAMESPACE PASSWORD 'app' ROLES EDITOR;
