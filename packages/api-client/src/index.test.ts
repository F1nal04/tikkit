import { expect, test } from "vitest";
import { ApiError, createClient } from "./index";

const stub = (status: number, body: string) => {
  const calls: { url: string; init: RequestInit }[] = [];
  const fetch = async (url: string | URL | Request, init?: RequestInit) => {
    calls.push({ url: String(url), init: init ?? {} });
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  };
  return { calls, fetch: fetch as typeof globalThis.fetch };
};

test("returns parsed JSON and sends language and credentials", async () => {
  const { calls, fetch } = stub(200, '{"status":"ok"}');
  const client = createClient({ fetch, baseUrl: "http://api.test", language: "de" });
  expect(await client.request("get", "/api/health")).toEqual({ status: "ok" });
  expect(calls[0].url).toBe("http://api.test/api/health");
  expect(calls[0].init.credentials).toBe("include");
  expect((calls[0].init.headers as Record<string, string>)["accept-language"]).toBe("de");
});

test("parses the error envelope into ApiError", async () => {
  const envelope = {
    error: {
      code: "validation_failed",
      message: "Some fields are invalid.",
      details: { fields: { name: ["required"] } },
    },
    request_id: "req-1",
  };
  const { fetch } = stub(422, JSON.stringify(envelope));
  const error = await createClient({ fetch }).request("get", "/api/ready").catch((e) => e);
  expect(error).toBeInstanceOf(ApiError);
  expect(error.status).toBe(422);
  expect(error.code).toBe("validation_failed");
  expect(error.requestId).toBe("req-1");
  expect(error.fields).toEqual({ name: ["required"] });
});

test("non-envelope errors keep the status without a code", async () => {
  const { fetch } = stub(502, '"Bad Gateway"');
  const error = await createClient({ fetch }).request("get", "/api/health").catch((e) => e);
  expect(error).toBeInstanceOf(ApiError);
  expect([error.status, error.code, error.body]).toEqual([502, null, null]);
});
