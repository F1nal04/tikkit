import Link from "next/link";
import { Button } from "@/components/ui/button";

export default function Home() {
  return (
    <div className="min-h-screen bg-gradient-to-br from-black via-neutral-900 to-red-900 text-white flex flex-col relative">
      {/* Main Content */}
      <main className="flex-1 container mx-auto px-4 flex items-center justify-center relative z-10">
        <div className="max-w-2xl mx-auto text-center">
          <h1 className="text-5xl font-bold mb-4 bg-gradient-to-r from-white to-red-400 bg-clip-text text-transparent">
            tikkit
          </h1>
          <p className="text-xl text-gray-300 mb-12">
            Family Ticket Management
          </p>

          <div className="flex flex-col sm:flex-row gap-4 justify-center">
            <Button
              asChild
              size="lg"
              className="bg-red-800 hover:bg-red-700 text-white px-8 py-4 text-lg"
            >
              <Link href="/create">Create Ticket</Link>
            </Button>
            <Button
              asChild
              variant="outline"
              size="lg"
              className="border-gray-600 text-gray-300 hover:bg-gray-800 px-8 py-4 text-lg"
            >
              <Link href="/tickets">View Tickets</Link>
            </Button>
          </div>
        </div>
      </main>

      {/* Footer */}
      <footer className="border-t border-gray-800 bg-black relative z-10">
        <div className="container mx-auto px-4 py-8">
          <div className="flex justify-center">
            <div className="flex flex-wrap gap-6 text-sm">
              <Link
                href="/privacy"
                className="text-gray-400 hover:text-red-400 transition-colors"
              >
                Privacy
              </Link>
              <Link
                href="/legal"
                className="text-gray-400 hover:text-red-400 transition-colors"
              >
                Legal Notice
              </Link>
              <a
                href="https://github.com/f1nal04/tikkit"
                target="_blank"
                rel="noopener noreferrer"
                className="text-gray-400 hover:text-red-400 transition-colors"
              >
                GitHub
              </a>
            </div>
          </div>
        </div>
      </footer>
    </div>
  );
}
