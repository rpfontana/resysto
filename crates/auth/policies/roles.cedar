@id("tenant/admin")
permit (principal in Resysto::Role::"tenant/admin", action, resource);

@id("tenant/analyst/read")
permit (
    principal in Resysto::Role::"tenant/analyst",
    action == Resysto::Action::"read",
    resource
);

// everything but the organization tree
@id("tenant/analyst/people")
permit (
    principal in Resysto::Role::"tenant/analyst",
    action,
    resource == Resysto::Resource::"people"
);

@id("tenant/reader")
permit (
    principal in Resysto::Role::"tenant/reader",
    action == Resysto::Action::"read",
    resource
);

@id("organizations/whole-tenant")
forbid (
    principal,
    action in [Resysto::Action::"write", Resysto::Action::"delete"],
    resource == Resysto::Resource::"organizations"
)
unless { principal.whole_tenant };
