import { Slot } from '@radix-ui/react-slot';
import { cva, type VariantProps } from 'class-variance-authority';
import type * as React from 'react';

import { cn } from '@/lib/utils';

const buttonVariants = cva(
  [
    'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-lg text-sm',
    'font-medium transition-all duration-150 disabled:pointer-events-none disabled:opacity-50',
    "[&_svg]:pointer-events-none [&_svg:not([class*='size-'])]:size-4 shrink-0",
    '[&_svg]:shrink-0 outline-none focus-visible:ring-2',
    'focus-visible:ring-ring',
  ],
  {
    variants: {
      variant: {
        default: ['bg-primary text-primary-foreground', 'hover:bg-primary/90'],
        destructive: ['bg-destructive text-destructive-foreground', 'hover:opacity-90'],
        outline: [
          'border border-border bg-transparent',
          'hover:bg-surface-hover hover:text-foreground',
        ],
        secondary: ['bg-surface-hover text-secondary-foreground', 'hover:text-foreground'],
        ghost: ['hover:bg-surface-hover text-muted-foreground', 'hover:text-foreground'],
        link: 'text-primary underline-offset-4 hover:underline',
      },
      size: {
        default: 'h-11 px-5',
        sm: 'h-9 px-4 text-sm',
        lg: 'h-12 px-6',
        icon: 'size-11',
        'icon-sm': 'size-9',
        'icon-lg': 'size-12',
      },
    },
    defaultVariants: {
      variant: 'default',
      size: 'default',
    },
  }
);

function Button({
  className,
  variant = 'default',
  size = 'default',
  asChild = false,
  ...props
}: React.ComponentProps<'button'> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean;
  }) {
  const Comp = asChild ? Slot : 'button';

  return (
    <Comp
      data-slot="button"
      data-variant={variant}
      data-size={size}
      className={cn(buttonVariants({ variant, size, className }))}
      {...props}
    />
  );
}

export { Button, buttonVariants };
