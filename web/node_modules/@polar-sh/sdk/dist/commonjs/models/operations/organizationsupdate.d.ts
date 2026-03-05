import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { OrganizationUpdate, OrganizationUpdate$Outbound } from "../components/organizationupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type OrganizationsUpdateRequest = {
    id: string;
    organizationUpdate: OrganizationUpdate;
};
/** @internal */
export declare const OrganizationsUpdateRequest$inboundSchema: z.ZodType<OrganizationsUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type OrganizationsUpdateRequest$Outbound = {
    id: string;
    OrganizationUpdate: OrganizationUpdate$Outbound;
};
/** @internal */
export declare const OrganizationsUpdateRequest$outboundSchema: z.ZodType<OrganizationsUpdateRequest$Outbound, z.ZodTypeDef, OrganizationsUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace OrganizationsUpdateRequest$ {
    /** @deprecated use `OrganizationsUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<OrganizationsUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `OrganizationsUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<OrganizationsUpdateRequest$Outbound, z.ZodTypeDef, OrganizationsUpdateRequest>;
    /** @deprecated use `OrganizationsUpdateRequest$Outbound` instead. */
    type Outbound = OrganizationsUpdateRequest$Outbound;
}
export declare function organizationsUpdateRequestToJSON(organizationsUpdateRequest: OrganizationsUpdateRequest): string;
export declare function organizationsUpdateRequestFromJSON(jsonString: string): SafeParseResult<OrganizationsUpdateRequest, SDKValidationError>;
//# sourceMappingURL=organizationsupdate.d.ts.map