// Client API. Access token chỉ giữ trong bộ nhớ; refresh token nằm trong cookie httpOnly.
const BASE: string =
  (import.meta.env.VITE_API_URL as string | undefined) ??
  "http://localhost:8080/api/v1";

let accessToken: string | null = null;

export function setAccessToken(token: string | null) {
  accessToken = token;
}

export class ApiError extends Error {
  status: number;
  code: string;
  constructor(status: number, code: string, message: string) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

async function raw(path: string, init: RequestInit): Promise<Response> {
  const headers = new Headers(init.headers);
  if (init.body && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  if (accessToken) headers.set("Authorization", `Bearer ${accessToken}`);
  return fetch(`${BASE}${path}`, {
    ...init,
    headers,
    credentials: "include",
  });
}

async function refreshAccessToken(): Promise<boolean> {
  const res = await fetch(`${BASE}/auth/refresh`, {
    method: "POST",
    credentials: "include",
    headers: { "X-Requested-With": "cho-cong-dong" },
  });
  if (!res.ok) {
    accessToken = null;
    return false;
  }
  const data = (await res.json()) as { access_token: string };
  accessToken = data.access_token;
  return true;
}

export async function api<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  let res = await raw(path, init);
  if (res.status === 401 && !path.startsWith("/auth/")) {
    if (await refreshAccessToken()) res = await raw(path, init);
  }
  if (res.status === 204) return undefined as T;
  const body: unknown = await res.json().catch(() => null);
  if (!res.ok) {
    const err = (body as { error?: { code?: string; message?: string } } | null)
      ?.error;
    throw new ApiError(
      res.status,
      err?.code ?? "unknown",
      err?.message ?? `Lỗi ${res.status}`,
    );
  }
  return body as T;
}

export function jsonBody(value: unknown): RequestInit {
  return { body: JSON.stringify(value) };
}
