const service = process.argv[2];

type ServiceOptions = {
  command: string[];
  cwd?: string;
};

const services: Record<string, ServiceOptions> = {
  api: {
    command: ["fabro", "serve", "--host", "0.0.0.0"],
  },
  web: {
    command: ["bun", "run", "dev", "--host", "0.0.0.0"],
    cwd: "apps/fabro-web",
  },
  docs: {
    command: ["mintlify", "dev", "--port", "3333", "--host", "0.0.0.0"],
    cwd: "docs",
  },
};

const validServices = Object.keys(services).join("|");

if (!service) {
  console.error(`Usage: docker run fabro <${validServices}>`);
  process.exit(1);
}

const config = services[service];
if (!config) {
  console.error(
    `Unknown service: ${service}. Use one of: ${Object.keys(services).join(", ")}`,
  );
  process.exit(1);
}

const child = Bun.spawn(config.command, {
  stdout: "inherit",
  stderr: "inherit",
  stdin: "inherit",
  cwd: config.cwd,
});

process.exit(await child.exited);
