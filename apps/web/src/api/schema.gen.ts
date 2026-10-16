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
        /**
         * @description Where a ticket currently is, as shown to its requester.
         * @enum {string}
         */
        AssignmentQueue: "general_queue" | "team";
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
        ErrorCode: "invalid_request" | "validation_failed" | "unauthenticated" | "forbidden" | "csrf_rejected" | "not_found" | "method_not_allowed" | "conflict" | "invalid_credentials" | "setup_completed" | "payload_too_large" | "rate_limited" | "internal_error" | "service_unavailable";
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
