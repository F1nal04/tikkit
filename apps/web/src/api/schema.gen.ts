// Generated from apps/api/openapi.json by openapi-typescript. Do not edit.
// Regenerate: bun run --cwd apps/web generate:api

export interface paths {
    "/api/health": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Liveness probe. */
        get: operations["getHealth"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/health/live": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Liveness probe (explicit path for orchestrators). */
        get: operations["getHealthLive"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/health/ready": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Readiness probe. Unlike other endpoints, a 503 here carries the
         *     `Readiness` body (not `ErrorBody`) so orchestrators and operators can see
         *     which check failed.
         */
        get: operations["getHealthReady"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/openapi.json": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** The OpenAPI document of this API. */
        get: operations["getOpenApi"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/accounts": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Lists accounts ordered by normalized username. */
        get: operations["listAdminAccounts"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/accounts/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** One account. */
        get: operations["getAdminAccount"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/accounts/{id}/role": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Changes an account's role. Takes effect on the account's next request. */
        patch: operations["changeAccountRole"];
        trace?: never;
    };
    "/api/v1/admin/teams": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Lists teams ordered by name (not paginated; teams are few). */
        get: operations["listTeams"];
        put?: never;
        /** Creates a team and appoints its lead atomically. */
        post: operations["createTeam"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** One team (active or archived). */
        get: operations["getTeam"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** Renames an active team. */
        patch: operations["renameTeam"];
        trace?: never;
    };
    "/api/v1/admin/teams/{id}/archive": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Archives a team and ends all memberships. Refused while open tickets remain. */
        post: operations["archiveTeam"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{id}/lead": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Replaces the team's lead atomically (never co-leads, never leaderless). */
        post: operations["replaceTeamLead"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{id}/members": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Adds a plain member. */
        post: operations["addTeamMember"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{id}/members/{accountId}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Removes a plain member (the lead must be replaced first). Clears the
         *     member's open assignments in this team.
         */
        delete: operations["removeTeamMember"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/login": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Signs in with username and password. Always creates a new session; a
         *     session cookie sent with the request is revoked (session fixation
         *     defence). Unknown usernames, wrong passwords and deactivated accounts get
         *     the same `401 invalid_credentials`.
         */
        post: operations["login"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/logout": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Signs out: revokes the presented session server-side and clears the
         *     cookie. Idempotent (204 without a session too).
         */
        post: operations["logout"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/auth/session": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * The current session. This is the source of truth for the signed-in UI
         *     state (e.g. after a page refresh).
         */
        get: operations["getSession"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/setup": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Whether first setup is still open. */
        get: operations["getSetupStatus"];
        put?: never;
        /**
         * Creates the initial Admin account and signs it in. One-time: after it
         *     succeeded (or concurrently with a winning attempt) it answers `409
         *     setup_completed`. All later accounts require an invitation.
         */
        post: operations["completeSetup"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        /** @description The signed-in account's own view of itself. */
        Account: {
            /** Format: date-time */
            createdAt: string;
            displayName: string;
            /** @description Contact address only; not unique and never used to identify accounts. */
            email: string;
            /** Format: uuid */
            id: string;
            language: components["schemas"]["Language"];
            role: components["schemas"]["Role"];
            /** @description Specialist field designation; grants no access. */
            specialization: string | null;
            status: components["schemas"]["AccountStatus"];
            team: null | components["schemas"]["AccountTeam"];
            /** @description As entered at creation (trimmed); login compares the normalized form. */
            username: string;
        };
        /**
         * @description Account lifecycle status.
         * @enum {string}
         */
        AccountStatus: "active" | "deactivated";
        AccountTeam: {
            /** Format: uuid */
            id: string;
            isLead: boolean;
            name: string;
        };
        AddMemberRequest: {
            /**
             * Format: uuid
             * @description An active Operator, Specialist or Team Lead account without a team.
             */
            accountId: string;
        };
        /**
         * @description Admin view of an account. Accounts are identified by `id` and `username`;
         *     `email` is contact information only and may be shared by several accounts.
         */
        AdminAccount: {
            /** Format: date-time */
            createdAt: string;
            /** Format: date-time */
            deactivatedAt: string | null;
            displayName: string;
            email: string;
            /** Format: uuid */
            id: string;
            language: components["schemas"]["Language"];
            role: components["schemas"]["Role"];
            /** @description Display designation (Specialists). Grants no access. */
            specialization: string | null;
            status: components["schemas"]["AccountStatus"];
            team: null | components["schemas"]["AccountTeam"];
            /** Format: date-time */
            updatedAt: string;
            username: string;
        };
        /**
         * @description Where a ticket currently is, as shown to its requester.
         * @enum {string}
         */
        AssignmentQueue: "general_queue" | "team";
        ChangeRoleRequest: {
            /**
             * Format: uuid
             * @description Required when the account currently leads a team and the new role is
             *     not `team_lead`: this Team Lead account takes over in the same change.
             */
            replacementLeadId?: string | null;
            role: components["schemas"]["Role"];
            /** @description Specialists only (1–100 characters); omit or `null` to clear. */
            specialization?: string | null;
        };
        CreateTeamRequest: {
            /**
             * Format: uuid
             * @description An active Team Lead account without a current team.
             */
            leadAccountId: string;
            /** @description 1–100 characters after trimming; unique among active teams. */
            name: string;
        };
        /** @enum {string} */
        DatabaseCheck: "ok" | "unavailable";
        /** @description Response body wrapper for every error: `{"error": {...}}`. */
        ErrorBody: {
            error: components["schemas"]["ErrorDetail"];
        };
        /**
         * @description Stable, language-independent error codes. Serialized as snake_case.
         *
         *     Adding a variant is a contract change: update `openapi.json`
         *     (`UPDATE_OPENAPI=1 cargo test --locked -p tikkit-api openapi`), regenerate
         *     the web client and add English and German messages in
         *     `apps/web/src/api/errors.ts` (the TypeScript build fails until you do).
         * @enum {string}
         */
        ErrorCode: "invalid_request" | "validation_failed" | "unauthenticated" | "forbidden" | "csrf_rejected" | "not_found" | "method_not_allowed" | "conflict" | "invalid_credentials" | "setup_completed" | "last_admin" | "team_membership_conflict" | "team_lead_required" | "team_has_open_tickets" | "payload_too_large" | "rate_limited" | "internal_error" | "service_unavailable";
        ErrorDetail: {
            code: components["schemas"]["ErrorCode"];
            fields?: components["schemas"]["FieldError"][];
            /** @description Safe English fallback message. Never contains internal details. */
            message: string;
            /** @description Same value as the `x-request-id` response header. */
            requestId: string;
        };
        /**
         * @description One invalid field. `field` uses the request's camelCase names with `.`
         *     for nesting and `[n]` for array items (e.g. `screenshots[0].name`).
         */
        FieldError: {
            code: components["schemas"]["FieldErrorCode"];
            field: string;
            /** @description Interpolation values for the translated message (e.g. `{"min": 3}`). */
            params?: {
                [key: string]: unknown;
            };
        };
        /**
         * @description Stable per-field validation codes. Serialized as snake_case.
         * @enum {string}
         */
        FieldErrorCode: "required" | "too_short" | "too_long" | "out_of_range" | "invalid_format" | "invalid_value" | "invalid_type" | "unknown_field" | "already_taken";
        Health: {
            /**
             * @description Always `ok` while the process serves requests.
             * @example ok
             */
            status: string;
        };
        /**
         * @description Interface/notification language. English is the fallback. A language
         *     choice never influences authorization or business rules.
         * @enum {string}
         */
        Language: "en" | "de";
        LoginRequest: {
            password: string;
            username: string;
        };
        /** @description One page of results. */
        Page_AdminAccount: {
            items: {
                /** Format: date-time */
                createdAt: string;
                /** Format: date-time */
                deactivatedAt: string | null;
                displayName: string;
                email: string;
                /** Format: uuid */
                id: string;
                language: components["schemas"]["Language"];
                role: components["schemas"]["Role"];
                /** @description Display designation (Specialists). Grants no access. */
                specialization: string | null;
                status: components["schemas"]["AccountStatus"];
                team: null | components["schemas"]["AccountTeam"];
                /** Format: date-time */
                updatedAt: string;
                username: string;
            }[];
            /** @description Pass as `cursor` to fetch the next page; `null` on the last page. */
            nextCursor: string | null;
        };
        /**
         * @description What happens to the previous lead when leadership is handed over.
         * @enum {string}
         */
        PreviousLead: "keep_as_member" | "remove_from_team";
        PublicAssignee: {
            displayName: string;
        };
        PublicTeam: {
            name: string;
        };
        /**
         * @description Public assignment details for requester-facing ticket responses.
         *
         *     Deliberately contains only display values: no team/account ids,
         *     usernames, emails, or roles. `team` and `assignee` are always present and
         *     `null` when not set, so "general queue" and "unassigned" are explicit.
         */
        PublicTicketAssignment: {
            assignee: null | components["schemas"]["PublicAssignee"];
            queue: components["schemas"]["AssignmentQueue"];
            team: null | components["schemas"]["PublicTeam"];
        };
        Readiness: {
            checks: components["schemas"]["ReadinessChecks"];
            /**
             * @description `ready` or `not_ready`.
             * @example ready
             */
            status: string;
        };
        ReadinessChecks: {
            database: components["schemas"]["DatabaseCheck"];
            schema: components["schemas"]["SchemaCheck"];
        };
        RenameTeamRequest: {
            name: string;
        };
        ReplaceLeadRequest: {
            /**
             * Format: uuid
             * @description An active Team Lead account in no team or already in this team.
             */
            accountId: string;
            previousLead?: null | components["schemas"]["PreviousLead"];
        };
        /**
         * @description Account role. Exactly one per account; see spec.md "Roles".
         * @enum {string}
         */
        Role: "user" | "operator" | "specialist" | "team_lead" | "admin";
        /** @enum {string} */
        SchemaCheck: "ok" | "pending_migrations" | "incompatible" | "unknown";
        /** @description The current session: who is signed in and until when. */
        SessionInfo: {
            account: components["schemas"]["Account"];
            /**
             * Format: date-time
             * @description Absolute expiry of the session (UTC). The session also ends after 12
             *     hours without activity.
             */
            expiresAt: string;
        };
        SetupRequest: {
            displayName: string;
            /** @description Contact address; not unique. */
            email: string;
            language: components["schemas"]["Language"];
            /** @description 12–256 characters, not equal to the username. */
            password: string;
            /**
             * @description 3–32 characters of `[a-z0-9._-]` after trimming, NFKC normalization
             *     and lowercasing. Stored as entered (trimmed) for display.
             */
            username: string;
        };
        SetupStatus: {
            /**
             * @description `true` until the initial Admin has been created. Never becomes `true`
             *     again.
             */
            setupRequired: boolean;
        };
        /**
         * @description Admin view of a team with its designated lead and current members
         *     (ordered by username). Archived teams have no lead and no members.
         */
        Team: {
            /** Format: date-time */
            archivedAt: string | null;
            /** Format: date-time */
            createdAt: string;
            /** Format: uuid */
            id: string;
            lead: null | components["schemas"]["TeamPerson"];
            members: components["schemas"]["TeamMember"][];
            name: string;
            status: components["schemas"]["TeamStatus"];
        };
        /** @description A current member (including the lead). */
        TeamMember: {
            displayName: string;
            /** Format: uuid */
            id: string;
            isLead: boolean;
            /** Format: date-time */
            memberSince: string;
            role: components["schemas"]["Role"];
            specialization: string | null;
            status: components["schemas"]["AccountStatus"];
            username: string;
        };
        /** @description Compact account reference (lead). */
        TeamPerson: {
            displayName: string;
            /** Format: uuid */
            id: string;
            username: string;
        };
        /** @enum {string} */
        TeamStatus: "active" | "archived";
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    getHealth: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The API process is up */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Health"];
                };
            };
        };
    };
    getHealthLive: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The API process is up */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Health"];
                };
            };
        };
    };
    getHealthReady: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Database reachable and schema compatible */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Readiness"];
                };
            };
            /** @description A required dependency is not ready */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Readiness"];
                };
            };
        };
    };
    getOpenApi: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description OpenAPI 3.1 document */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": Record<string, never>;
                };
            };
        };
    };
    listAdminAccounts: {
        parameters: {
            query?: {
                /** @description Opaque cursor from a previous response's `nextCursor`. */
                cursor?: string;
                /** @description Page size (1–100, default 25). */
                limit?: number;
                /** @description Case-insensitive substring of username, display name or email (max 100). */
                q?: string;
                /** @description Filter by role. */
                role?: components["schemas"]["Role"];
                /** @description Filter by status. */
                status?: components["schemas"]["AccountStatus"];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of accounts */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Page_AdminAccount"];
                };
            };
            /** @description `invalid_request` (unknown parameter) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` (not an Admin) */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` (role, status, q, limit, cursor) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getAdminAccount: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Account id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The account */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AdminAccount"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` (not an Admin) */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    changeAccountRole: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Account id */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ChangeRoleRequest"];
            };
        };
        responses: {
            /** @description The updated account */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AdminAccount"];
                };
            };
            /** @description `invalid_request` (unknown role or field) */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `last_admin`, `team_membership_conflict`, `team_lead_required`, `conflict` */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` (specialization, replacementLeadId, accountId) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    listTeams: {
        parameters: {
            query?: {
                /** @description Include archived teams (default `false`). */
                includeArchived?: boolean;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Teams with lead and current members */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"][];
                };
            };
            /** @description `invalid_request` */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` (not an Admin) */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    createTeam: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateTeamRequest"];
            };
        };
        responses: {
            /** @description The new team */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `invalid_request` */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `team_membership_conflict` (the lead already belongs to a team) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` (name, leadAccountId) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The team */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` (not an Admin) */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    renameTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team id */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RenameTeamRequest"];
            };
        };
        responses: {
            /** @description The team */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `invalid_request` */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `conflict` (archived) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` (name) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    archiveTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The archived team */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `team_has_open_tickets`, `conflict` (already archived) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    replaceTeamLead: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team id */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReplaceLeadRequest"];
            };
        };
        responses: {
            /** @description The team */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `invalid_request` */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `team_membership_conflict`, `conflict` */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` (accountId) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    addTeamMember: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team id */
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddMemberRequest"];
            };
        };
        responses: {
            /** @description The team */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `invalid_request` */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `team_membership_conflict`, `conflict` (archived) */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` (accountId) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    removeTeamMember: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Account id of the member */
                accountId: string;
                /** @description Team id */
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The team */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Team"];
                };
            };
            /** @description `unauthenticated` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `forbidden` / `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `not_found` (team or membership) */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `team_lead_required` */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    login: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["LoginRequest"];
            };
        };
        responses: {
            /** @description Signed in; sets the session cookie */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SessionInfo"];
                };
            };
            /** @description Malformed request */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `invalid_credentials` */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `rate_limited` (see `Retry-After`) */
            429: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    logout: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Signed out; clears the session cookie */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getSession: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Signed in */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SessionInfo"];
                };
            };
            /** @description `unauthenticated` (an invalid cookie is cleared) */
            401: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    getSetupStatus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Setup state */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SetupStatus"];
                };
            };
            /** @description Database unavailable */
            503: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
    completeSetup: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetupRequest"];
            };
        };
        responses: {
            /** @description Admin created and signed in; sets the session cookie */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SessionInfo"];
                };
            };
            /** @description Malformed request */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `csrf_rejected` */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `setup_completed` */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `validation_failed` */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
            /** @description `rate_limited` (see `Retry-After`) */
            429: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ErrorBody"];
                };
            };
        };
    };
}
