import * as commands from "wendy:commands";

/** A CLI command, independent of the model's tool catalog. */
export async function onLoad() {
  await commands.register({
    name: "hello",
    description: "向指定名字问好",
    handler: (input: { name?: string }) => {
      if (input?.name !== undefined && typeof input.name !== "string") throw new Error("name must be a string");
      return { message: `你好，${input?.name ?? "Wendy"}` };
    },
  });
}
