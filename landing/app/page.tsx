import { Features } from "@/components/features";
import { FinalCta } from "@/components/final-cta";
import { Footer } from "@/components/footer";
import { Hero } from "@/components/hero";
import { Nav } from "@/components/nav";
import { Speed } from "@/components/speed";

export default function Home() {
  return (
    <main className="min-h-screen overflow-hidden bg-base text-ink">
      <Nav />
      <Hero />
      <Features />
      <Speed />
      <FinalCta />
      <Footer />
    </main>
  );
}
