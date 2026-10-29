/** @type {import('next').NextConfig} */
const nextConfig = {
  // @coral-xyz/anchor pulls in node built-ins; keep it server-side only.
  serverExternalPackages: ["@coral-xyz/anchor"],
};
export default nextConfig;
