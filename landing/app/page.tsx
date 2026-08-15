import { AiWorkflows } from "@/components/ai-workflows";
import { AsyncEngine } from "@/components/async-engine";
import { Capabilities } from "@/components/capabilities";
import { FinalCta } from "@/components/final-cta";
import { Footer } from "@/components/footer";
import { Hero } from "@/components/hero";
import { Nav } from "@/components/nav";
import { Philosophy } from "@/components/philosophy";
import { SignalStrip } from "@/components/signal-strip";
import { Surfaces } from "@/components/surfaces";
import { Trust } from "@/components/trust";

export default function Home() {
  return (
    <main className="min-h-screen overflow-hidden bg-base text-ink">
      <Nav />
      <Hero />
      <SignalStrip />
      <Philosophy />
      <AsyncEngine />
      <AiWorkflows />
      <Capabilities />
      <Surfaces />
      <Trust />
      <FinalCta />
      <Footer />
    </main>
  );
}
