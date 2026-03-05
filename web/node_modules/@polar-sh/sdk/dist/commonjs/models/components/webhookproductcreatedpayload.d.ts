import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Product, Product$Outbound } from "./product.js";
/**
 * Sent when a new product is created.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookProductCreatedPayload = {
    type?: "product.created" | undefined;
    /**
     * A product.
     */
    data: Product;
};
/** @internal */
export declare const WebhookProductCreatedPayload$inboundSchema: z.ZodType<WebhookProductCreatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookProductCreatedPayload$Outbound = {
    type: "product.created";
    data: Product$Outbound;
};
/** @internal */
export declare const WebhookProductCreatedPayload$outboundSchema: z.ZodType<WebhookProductCreatedPayload$Outbound, z.ZodTypeDef, WebhookProductCreatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookProductCreatedPayload$ {
    /** @deprecated use `WebhookProductCreatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookProductCreatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookProductCreatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookProductCreatedPayload$Outbound, z.ZodTypeDef, WebhookProductCreatedPayload>;
    /** @deprecated use `WebhookProductCreatedPayload$Outbound` instead. */
    type Outbound = WebhookProductCreatedPayload$Outbound;
}
export declare function webhookProductCreatedPayloadToJSON(webhookProductCreatedPayload: WebhookProductCreatedPayload): string;
export declare function webhookProductCreatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookProductCreatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookproductcreatedpayload.d.ts.map