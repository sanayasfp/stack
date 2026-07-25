const { createServer } = require("node:http");

const port = process.env.PORT || 3000;

const server = createServer((req, res) => {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(JSON.stringify({ message: "hello from node-api, routed through stack" }));
});

server.listen(port, () => {
  console.log(`node-api listening on port ${port}`);
});
