import type { ApiErrorBody, MetaResponse, PlanMapResponse } from "../types/map";

const API_BASE = import.meta.env.VITE_API_BASE ?? "";

async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, init);
  if (!res.ok) {
    let message = `${res.status} ${res.statusText}`;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body.error) message = body.error;
    } catch {
      // ignore
    }
    throw new Error(message);
  }
  return (await res.json()) as T;
}

export function fetchMeta(): Promise<MetaResponse> {
  return apiFetch<MetaResponse>("/api/v1/meta");
}

export function fetchPlanMap(): Promise<PlanMapResponse> {
  return apiFetch<PlanMapResponse>("/api/v1/plans/latest/map");
}

export function reloadPlan(): Promise<{ reloaded: boolean }> {
  return apiFetch<{ reloaded: boolean }>("/api/v1/plans/reload", {
    method: "POST",
  });
}
