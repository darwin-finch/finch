import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Organization, Organization$Outbound } from "./organization.js";
/**
 * Sent when a organization is updated.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookOrganizationUpdatedPayload = {
    type?: "organization.updated" | undefined;
    data: Organization;
};
/** @internal */
export declare const WebhookOrganizationUpdatedPayload$inboundSchema: z.ZodType<WebhookOrganizationUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookOrganizationUpdatedPayload$Outbound = {
    type: "organization.updated";
    data: Organization$Outbound;
};
/** @internal */
export declare const WebhookOrganizationUpdatedPayload$outboundSchema: z.ZodType<WebhookOrganizationUpdatedPayload$Outbound, z.ZodTypeDef, WebhookOrganizationUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookOrganizationUpdatedPayload$ {
    /** @deprecated use `WebhookOrganizationUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookOrganizationUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookOrganizationUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookOrganizationUpdatedPayload$Outbound, z.ZodTypeDef, WebhookOrganizationUpdatedPayload>;
    /** @deprecated use `WebhookOrganizationUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookOrganizationUpdatedPayload$Outbound;
}
export declare function webhookOrganizationUpdatedPayloadToJSON(webhookOrganizationUpdatedPayload: WebhookOrganizationUpdatedPayload): string;
export declare function webhookOrganizationUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookOrganizationUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookorganizationupdatedpayload.d.ts.map