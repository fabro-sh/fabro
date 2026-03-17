import { defineCollection, z } from "astro:content";
import { glob } from "astro/loaders";

const roadmap = defineCollection({
  loader: glob({ pattern: "**/*.yaml", base: "./src/content/roadmap" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    status: z.enum(["shipped", "building", "next"]),
    date: z.coerce.date(),
  }),
});

const blog = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/blog" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    date: z.coerce.date(),
    author: z.string(),
  }),
});

const showcase = defineCollection({
  loader: glob({ pattern: "**/*.md", base: "./src/content/showcase" }),
  schema: z.object({
    title: z.string(),
    description: z.string(),
    thumbnail: z.string(),
    tags: z.array(z.string()),
    languages: z.array(z.enum(["python", "rust", "typescript", "ruby"])),
    github: z.string(),
    models: z.array(z.string()),
    skills: z.array(z.string()),
    prompt: z.string(),
    workflow: z.string(),
    sortOrder: z.number(),
  }),
});

export const collections = { roadmap, blog, showcase };
