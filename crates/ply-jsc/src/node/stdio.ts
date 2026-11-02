import { readStdin, toBuffer } from "ply:runtime";
import { Readable } from "node:stream";

let reading = false;
export const stdin = new Readable({
  read() {
    if (reading) return;
    reading = true;
    void readStdin().then(bytes => {
      reading = false;
      this.push(bytes.length ? toBuffer(bytes) : null);
    }, (error: Error) => { reading = false; this.destroy(error); });
  },
});
