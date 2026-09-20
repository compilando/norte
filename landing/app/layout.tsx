import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import "./globals.css";

const inter = Inter({
  subsets: ["latin"],
  variable: "--font-inter",
  display: "swap",
});

const jetBrainsMono = JetBrains_Mono({
  subsets: ["latin"],
  variable: "--font-jetbrains",
  display: "swap",
});

const ogImage = "https://raw.githubusercontent.com/compilando/norte/main/landing/public/norte-aurora.png";

export const metadata: Metadata = {
  title: "Norte — The open-source file commander for the agent era",
  description:
    "One asynchronous Rust core for every file, terminal, window and AI agent. Local, SFTP, FTP, S3 and archives—with review, policy, journal and undo. No account. No telemetry.",
  keywords: [
    "file manager",
    "orthodox file manager",
    "terminal file manager",
    "TUI",
    "Rust",
    "open source",
    "MCP",
    "AI agents",
    "SFTP",
    "S3",
  ],
  authors: [{ name: "Norte contributors" }],
  openGraph: {
    type: "website",
    siteName: "Norte",
    title: "Norte — Your files have a new sense of direction.",
    description:
      "The open-source, AI-native file commander. A terminal and a window over one governed Rust core. Async by design. Reversible by default.",
    images: [{ url: ogImage, width: 1200, height: 630, alt: "Norte" }],
  },
  twitter: {
    card: "summary_large_image",
    title: "Norte — Your files have a new sense of direction.",
    description: "The open-source, AI-native file commander. Async by design. Reversible by default.",
    images: [ogImage],
  },
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" className="scroll-smooth" suppressHydrationWarning>
      <body className={`${inter.variable} ${jetBrainsMono.variable}`}>{children}</body>
    </html>
  );
}
