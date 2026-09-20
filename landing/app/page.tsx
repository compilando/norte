import { AiWorkflows } from "@/components/ai-workflows";
import { AsyncEngine } from "@/components/async-engine";
import { Capabilities } from "@/components/capabilities";
import { FinalCta } from "@/components/final-cta";
import { Footer } from "@/components/footer";
import { Goto } from "@/components/goto";
import { Hero } from "@/components/hero";
import { MakeItYours } from "@/components/make-it-yours";
import { Nav } from "@/components/nav";
import { Parity } from "@/components/parity";
import { Philosophy } from "@/components/philosophy";
import { SignalStrip } from "@/components/signal-strip";
import { Surfaces } from "@/components/surfaces";
import { Timeline } from "@/components/timeline";
import { Trust } from "@/components/trust";

export default function Home() {
  return (
    <main className="min-h-screen overflow-hidden bg-base text-ink">
      <Nav />
      <Hero />
      <SignalStrip />
      <Philosophy />
      <Parity />
      <AsyncEngine />
      <AiWorkflows />
      <Goto />
      <Timeline />
      <Capabilities />
      <MakeItYours />
      <Surfaces />
      <Trust />
      <FinalCta />
      <Footer />
    </main>
  );
}
