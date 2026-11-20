import { useSeoMeta } from "@unhead/react";
import { Link } from "react-router-dom";

import { Logo } from "@/components/Logo";
import { Button } from "@/components/ui/button";

export default function NotFound() {
  useSeoMeta({ title: "Page not found · Shakespeare AI" });

  return (
    <div className="flex flex-col items-center gap-4 py-16 text-center">
      <Logo className="size-16 opacity-90 grayscale-[30%]" />
      <h1 className="text-brand text-6xl font-bold tracking-tight">404</h1>
      <p className="text-muted-foreground">There's nothing at this address.</p>
      <Button asChild variant="outline" className="rounded-full px-5">
        <Link to="/">Go home</Link>
      </Button>
    </div>
  );
}
