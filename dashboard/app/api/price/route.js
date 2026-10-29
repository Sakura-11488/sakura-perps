import { readOracleState } from "../../../lib/chain";

// Lightweight, fast-poll endpoint: just the oracle price + guard verdict.
export const dynamic = "force-dynamic";
export const revalidate = 0;

export async function GET() {
  try {
    const oracle = await readOracleState();
    return Response.json(oracle ?? { error: "oracle unavailable" }, {
      headers: { "Cache-Control": "no-store" },
    });
  } catch (err) {
    return Response.json({ error: err.message }, { status: 500 });
  }
}
