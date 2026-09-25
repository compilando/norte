import "../globals.css";
import type { ReactNode } from "react";
import { metadataFor, Root } from "@/components/root";

export const metadata = metadataFor("en");

export default function Layout({ children }: { children: ReactNode }) {
  return <Root lang="en">{children}</Root>;
}
