import { readChainState } from "../../../lib/chain";

// Always read fresh on-chain state, never cache.
export const dynamic = "force-dynamic";
export const revalidate = 0;

export async function GET() {
  try {
    const state = await readChainState();
    return Response.json(state, {
      headers: { "Cache-Control": "no-store" },
    });
  } catch (err) {
    return Response.json({ error: err.message }, { status: 500 });
  }
}
