window.addEventListener("load", function () {
  window.SwaggerUIBundle({
    url: "../openapi.json",
    dom_id: "#swagger-ui",
    deepLinking: true,
    tryItOutEnabled: false,
    supportedSubmitMethods: [],
    defaultModelsExpandDepth: 0,
  });
});
