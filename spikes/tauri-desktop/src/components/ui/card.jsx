import { cn } from "../../lib/utils";

export function Card({ className, ...props }) {
  return (
    <div
      className={cn(
        "rounded-xl border border-border/70 bg-card/70 text-card-foreground shadow-[0_15px_60px_rgba(0,0,0,0.35)] backdrop-blur",
        className
      )}
      {...props}
    />
  );
}

export function CardHeader({ className, ...props }) {
  return <div className={cn("flex flex-col space-y-1.5 p-5", className)} {...props} />;
}

export function CardTitle({ className, ...props }) {
  return <h3 className={cn("text-lg font-semibold tracking-tight", className)} {...props} />;
}

export function CardContent({ className, ...props }) {
  return <div className={cn("p-5 pt-0", className)} {...props} />;
}
