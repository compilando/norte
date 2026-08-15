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

export const metadata: Metadata = {
  title: "Norte — The open-source file commander for the agent era",
  description: "One fast, asynchronous Rust core for every file, terminal and AI agent. Local, remote and cloud storage—with review, policy, journal and undo.",
  openGraph: {
    title: "Norte — Your files have a new sense of direction.",
    description: "The open-source, AI-native file commander. Async by design. Reversible by default.",
    images: ["https://raw.githubusercontent.com/compilando/norte/main/landing/public/norte-aurora.png"],
  },
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" className="scroll-smooth" suppressHydrationWarning>
      <body className={`${inter.variable} ${jetBrainsMono.variable}`}>{children}</body>
    </html>
  );
}
