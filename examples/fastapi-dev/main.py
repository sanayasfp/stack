from fastapi import FastAPI

app = FastAPI()


@app.get("/")
def read_root():
    return {"message": "hello from fastapi-dev, routed through stack"}


@app.get("/health")
def health():
    return {"status": "ok"}
