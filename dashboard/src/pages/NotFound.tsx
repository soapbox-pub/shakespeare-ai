import { useSeoMeta } from "@unhead/react";
import { Link } from "react-router-dom";

import { Button } from "@/components/ui/button";

export default function NotFound() {
  useSeoMeta({ title: "Page not found · Shakespeare AI" });

  return (
    <div className="flex flex-col items-center gap-4 py-16 text-center">
      <h1 className="text-4xl font-semibold tracking-tight">404</h1>
      <p className="text-muted-foreground">There's nothing at this address.</p>
      <Button asChild variant="outline">
        <Link to="/">Go home</Link>
      </Button>
    </div>
  );
}
