import { CommandStrip } from "@/components/command-strip";
import { Core } from "@/components/core";
import { Features } from "@/components/features";
import { FinalCta } from "@/components/final-cta";
import { Footer } from "@/components/footer";
import { Hero } from "@/components/hero";
import { Manifesto } from "@/components/manifesto";
import { Nav } from "@/components/nav";
import { Speed } from "@/components/speed";
import { Trust } from "@/components/trust";

export default function Home() {
  return (
    <main className="min-h-screen overflow-hidden bg-base text-ink">
      <Nav />
      <Hero />
      <CommandStrip />
      <Manifesto />
      <Features />
      <Speed />
      <Trust />
      <Core />
      <FinalCta />
      <Footer />
    </main>
  );
}
