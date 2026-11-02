import "server-only";

// Exception messages may embed RPC URLs, headers, provider responses or keys.
// Only route-owned constants cross the HTTP boundary; never serialize errors.
export async function readResponse(read, unavailableCode) {
  const headers = { "Cache-Control": "no-store" };
  try {
    const data = await read();
    if (data === null) {
      return Response.json({ error: unavailableCode }, { status: 503, headers });
    }
    return Response.json(data, { headers });
  } catch {
    return Response.json({ error: unavailableCode }, { status: 503, headers });
  }
}
