import json

from flask import Flask, Response, stream_with_context, jsonify

app = Flask(__name__)
def format_sse(data: str | None, event=None) -> str:
    if data:
        msg = f'data: {data}\n\n'
    else:
        msg = '\n'

    if event is not None:
        msg = f'event: {event}\n{msg}'

    return msg

@app.route('/')
def home():
    return jsonify({"results": [{"idx": 1}, {"idx": 2}]})

@app.route('/graphql/stream',  methods=['POST'])
def stream():
    print("sse stream called")
    @stream_with_context
    def eventStream():
        messages = ["Hi", "Bonjour", "Hola", "Ciao", "Zdravo"]
        for msg in messages:
            data = {
                "data": {
                    "greetings": msg
                }
            }
            yield format_sse(json.dumps(data), "next")

        yield format_sse(None, "complete")
    return Response(eventStream(), mimetype="text/event-stream")

if __name__ == '__main__':
    from waitress import serve
    from werkzeug.serving import WSGIRequestHandler
    WSGIRequestHandler.protocol_version = "HTTP/1.1"
    serve(app, host="127.0.0.1", port=4200)
    print("Server stopped.")