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
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
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
        ErrorCode: "invalid_request" | "validation_failed" | "unauthenticated" | "forbidden" | "csrf_rejected" | "not_found" | "method_not_allowed" | "conflict" | "payload_too_large" | "rate_limited" | "internal_error" | "service_unavailable";
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
        /** @enum {string} */
        SchemaCheck: "ok" | "pending_migrations" | "incompatible" | "unknown";
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
}
