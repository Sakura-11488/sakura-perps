import "./globals.css";

export const metadata = {
  title: "Sakura Perps — devnet monitor",
  description: "Read-only live view of the sakura-perps program on Solana devnet",
};

// Apply the saved theme before paint to avoid a flash of the wrong palette.
const themeInit = `(function(){try{var t=localStorage.getItem('theme');if(t)document.documentElement.dataset.theme=t;}catch(e){}})();`;

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>
        <script dangerouslySetInnerHTML={{ __html: themeInit }} />
        {children}
      </body>
    </html>
  );
}
