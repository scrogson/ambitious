# Simple GenServer for testing Rust interop
# Run with: elixir --sname elixir_test --cookie test_cookie test_server.exs

defmodule TestServer do
  use GenServer

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, :ok, opts)
  end

  @impl true
  def init(:ok) do
    IO.puts("TestServer started on #{Node.self()}")
    {:ok, %{counter: 0}}
  end

  @impl true
  def handle_call(:ping, _from, state) do
    IO.puts("Received :ping, replying :pong")
    {:reply, :pong, state}
  end

  @impl true
  def handle_call({:echo, msg}, _from, state) do
    IO.puts("Received echo request: #{inspect(msg)}")
    {:reply, {:ok, msg}, state}
  end

  @impl true
  def handle_call(:get_counter, _from, state) do
    {:reply, state.counter, state}
  end

  @impl true
  def handle_call(:increment, _from, state) do
    new_state = %{state | counter: state.counter + 1}
    {:reply, new_state.counter, new_state}
  end

  @impl true
  def handle_cast({:print, msg}, state) do
    IO.puts("Cast received: #{inspect(msg)}")
    {:noreply, state}
  end

  @impl true
  def handle_info(msg, state) do
    IO.puts("Info received: #{inspect(msg)}")
    {:noreply, state}
  end
end

# Start the server and register it
{:ok, pid} = TestServer.start_link(name: :test_server)
IO.puts("TestServer registered as :test_server with PID #{inspect(pid)}")
IO.puts("Node: #{Node.self()}")
IO.puts("Cookie: #{Node.get_cookie()}")
IO.puts("")
IO.puts("Waiting for connections... (Ctrl+C to exit)")
IO.puts("From Rust, connect to: #{Node.self()} with cookie 'test_cookie'")
IO.puts("")

# Keep the script running
Process.sleep(:infinity)
